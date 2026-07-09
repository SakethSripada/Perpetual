//! Provider-hosted cloud execution clients (Codex Cloud, Claude Code on the
//! web). Cloud runs are not streaming child processes: launches are one-shot
//! CLI submissions, progress is polled, and results come back through git (or
//! a provider diff). The orchestration around them lives in `am-core`; this
//! module owns only the provider command surfaces.

mod claude;
mod codex;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use am_proto::{AgentKind, CloudAvailability};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

pub use claude::ClaudeCloudClient;
pub use codex::CodexCloudClient;

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("{0} CLI is not installed")]
    NotInstalled(&'static str),
    #[error("{0}")]
    NotAuthenticated(String),
    #[error("usage limited{}", reset_at.map(|dt| format!(" (resets {dt})")).unwrap_or_default())]
    UsageLimited { reset_at: Option<DateTime<Utc>> },
    #[error("cloud launch failed: {0}")]
    Launch(String),
    #[error("cloud command failed: {0}")]
    Command(String),
    #[error("could not parse provider output: {0}")]
    Parse(String),
}

/// Everything a client needs to submit a continuation run.
#[derive(Debug, Clone)]
pub struct CloudLaunchRequest {
    /// Continuation prompt (context preamble + task objective).
    pub prompt: String,
    /// Worktree the launch command runs from. For Claude this binds the
    /// session to the worktree's repo and branch; for Codex it is the cwd.
    pub worktree: PathBuf,
    /// Branch the cloud run should work on (already pushed).
    pub branch: Option<String>,
    /// Codex Cloud environment id. Ignored by Claude.
    pub env_id: Option<String>,
}

/// Provider-side handle for a launched run.
#[derive(Debug, Clone)]
pub struct CloudTaskRef {
    pub agent: AgentKind,
    pub task_id: Option<String>,
    pub url: Option<String>,
    pub env_id: Option<String>,
}

/// Provider-reported state of a cloud run. `Unknown` means the provider has
/// no queryable status surface (Claude) or the task wasn't found; callers
/// fall back to git-based observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudPollStatus {
    Provisioning,
    Running,
    Completed,
    Failed(String),
    Expired,
    Unknown,
}

#[async_trait]
pub trait CloudTaskClient: Send + Sync {
    fn agent(&self) -> AgentKind;

    /// Probe whether a launch is expected to succeed. Repo-level requirements
    /// (pushed remote, worktree) are the caller's to check; this covers the
    /// CLI, auth, and provider configuration.
    async fn availability(&self, codex_env_id: Option<&str>) -> CloudAvailability;

    async fn launch(&self, req: &CloudLaunchRequest) -> Result<CloudTaskRef, CloudError>;

    async fn poll(&self, task: &CloudTaskRef) -> Result<CloudPollStatus, CloudError>;

    /// Pull provider-side results into the worktree (e.g. `codex apply`).
    /// Branch commits are fetched by the caller through `am-vcs`; clients only
    /// handle result surfaces git can't see. Returns a human-readable summary.
    async fn fetch_results(
        &self,
        task: &CloudTaskRef,
        worktree: &Path,
    ) -> Result<String, CloudError>;
}

/// The cloud client for a provider, when one exists.
pub fn cloud_client_for(agent: AgentKind) -> Option<Box<dyn CloudTaskClient>> {
    match agent {
        AgentKind::Codex => Some(Box::new(CodexCloudClient)),
        AgentKind::ClaudeCode => Some(Box::new(ClaudeCloudClient)),
        _ => None,
    }
}

/// Output of a finished (or timed-out) cloud CLI command.
pub(crate) struct CommandOutput {
    pub status_success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub(crate) fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Run a provider CLI command with discrete args (never a shell string),
/// capturing both streams, bounded by `timeout`.
pub(crate) async fn run_cloud_command(
    binary: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<CommandOutput, CloudError> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let fut = cmd.output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| CloudError::Command(format!("timed out after {}s", timeout.as_secs())))?
        .map_err(|e| CloudError::Command(e.to_string()))?;
    Ok(CommandOutput {
        status_success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Surface a usage-limit error if the provider output looks like one,
/// otherwise a generic launch error with the trimmed output.
pub(crate) fn launch_failure(output: &str) -> CloudError {
    if let Some(reset_at) = crate::limits::detect_usage_limit(output) {
        return CloudError::UsageLimited { reset_at };
    }
    let trimmed: String = output.trim().chars().take(600).collect();
    CloudError::Launch(if trimmed.is_empty() {
        "provider CLI exited with an error and no output".to_string()
    } else {
        trimmed
    })
}

/// First `https://` URL in provider output, trimmed of trailing punctuation.
pub(crate) fn extract_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        if let Some(start) = token.find("https://") {
            let url = token[start..]
                .trim_end_matches(['.', ',', ')', ']', ';', '"', '\''])
                .to_string();
            if url.len() > "https://".len() {
                return Some(url);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_url_from_noise() {
        let text = "Task created!\nView it at https://chatgpt.com/codex/tasks/task_e_abc123.\nDone";
        assert_eq!(
            extract_url(text).as_deref(),
            Some("https://chatgpt.com/codex/tasks/task_e_abc123")
        );
        assert_eq!(extract_url("no links here"), None);
    }

    #[test]
    fn launch_failure_classifies_usage_limits() {
        assert!(matches!(
            launch_failure("You've hit your usage limit. Try again at 2:30 pm."),
            CloudError::UsageLimited { .. }
        ));
        assert!(matches!(
            launch_failure("environment not found"),
            CloudError::Launch(_)
        ));
    }
}
