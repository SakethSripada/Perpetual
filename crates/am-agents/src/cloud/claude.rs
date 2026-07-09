//! Claude Code web-session client over the `claude` CLI.
//!
//! Verified against claude 2.1.199 (2026-07-03):
//! - `claude auth status` prints JSON: `{loggedIn, authMethod, apiProvider,
//!   subscriptionType, ...}`. Cloud sessions require `authMethod: "claude.ai"`
//!   (subscription auth); API-key/Bedrock/Vertex auth can't use them.
//! - `--cloud [description|session_id|url]` is the current hidden cloud-session
//!   flag. `--remote` still exists as a deprecated alias, and `--teleport`
//!   resumes cloud sessions.
//! - It creates a web session from the cwd's GitHub remote at the current
//!   branch (push first), bundling the repo when GitHub is missing.
//! - There is no documented headless list/status surface for web sessions;
//!   progress is observed through git (the session pushes to its branch, and
//!   commits carry a `Claude-Session: <url>` trailer). `poll` therefore
//!   returns `Unknown` and the orchestrator watches the branch.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use am_proto::{AgentKind, CloudAvailability};
use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{
    extract_url, launch_failure, run_cloud_command, CloudError, CloudLaunchRequest,
    CloudPollStatus, CloudTaskClient, CloudTaskRef,
};
use crate::detect::find_binary;

const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// Session creation includes cloning/provisioning; bundle uploads can be slow.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(300);
/// Once the session ref is known, give the CLI a moment to finish handing off
/// before we stop babysitting it. The session itself lives on Anthropic infra.
const POST_REF_GRACE: Duration = Duration::from_secs(20);

pub struct ClaudeCloudClient;

#[async_trait]
impl CloudTaskClient for ClaudeCloudClient {
    fn agent(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    async fn availability(&self, _codex_env_id: Option<&str>) -> CloudAvailability {
        let mut blockers = Vec::new();
        let mut authenticated = false;

        match find_binary("claude") {
            None => blockers.push("Claude Code CLI is not installed".to_string()),
            Some(bin) => {
                match run_cloud_command(&bin, &["auth", "status"], None, AUTH_TIMEOUT).await {
                    Ok(out) if out.status_success => {
                        match serde_json::from_str::<Value>(out.stdout.trim()) {
                            Ok(v) => {
                                let logged_in =
                                    v.get("loggedIn").and_then(Value::as_bool).unwrap_or(false);
                                let method = v
                                    .get("authMethod")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                authenticated = logged_in;
                                if !logged_in {
                                    blockers.push("Claude Code is not signed in".to_string());
                                } else if method != "claude.ai" {
                                    blockers.push(format!(
                                        "Claude web sessions need claude.ai subscription auth; current auth method is \"{method}\""
                                    ));
                                }
                            }
                            Err(_) => blockers
                                .push("could not parse `claude auth status` output".to_string()),
                        }
                    }
                    _ => blockers.push("Claude Code is not signed in".to_string()),
                }
            }
        }

        CloudAvailability {
            agent: AgentKind::ClaudeCode,
            ready: blockers.is_empty(),
            authenticated,
            blockers,
            checked_at: am_proto::now(),
        }
    }

    async fn launch(&self, req: &CloudLaunchRequest) -> Result<CloudTaskRef, CloudError> {
        let binary = find_binary("claude").ok_or(CloudError::NotInstalled("claude"))?;

        let mut cmd = tokio::process::Command::new(&binary);
        cmd.arg("--cloud")
            .arg(&req.prompt)
            .current_dir(&req.worktree)
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| CloudError::Command(e.to_string()))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        for reader in [stdout.map(ReaderKind::Out), stderr.map(ReaderKind::Err)]
            .into_iter()
            .flatten()
        {
            let tx = line_tx.clone();
            tokio::spawn(async move {
                match reader {
                    ReaderKind::Out(s) => forward_lines(s, tx).await,
                    ReaderKind::Err(s) => forward_lines(s, tx).await,
                }
            });
        }
        drop(line_tx);

        // Stream the launch output: capture everything for diagnostics and
        // stop early once a session ref is visible and the grace has passed.
        let mut all_output = String::new();
        let mut session: Option<(Option<String>, Option<String>)> = None;
        let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
        let mut grace_until: Option<tokio::time::Instant> = None;

        let exited = loop {
            let wait = grace_until
                .map(|g| g.min(deadline))
                .unwrap_or(deadline)
                .saturating_duration_since(tokio::time::Instant::now());
            tokio::select! {
                line = line_rx.recv() => match line {
                    Some(line) => {
                        all_output.push_str(&line);
                        all_output.push('\n');
                        if session.is_none() {
                            if let Some(found) = parse_session_ref(&line) {
                                session = Some(found);
                                grace_until =
                                    Some(tokio::time::Instant::now() + POST_REF_GRACE);
                            }
                        }
                    }
                    // Streams closed; wait for exit below.
                    None => break false,
                },
                status = child.wait() => break status.map(|s| s.success()).unwrap_or(false),
                _ = tokio::time::sleep(wait) => {
                    // Grace or overall deadline expired with the CLI still
                    // running. The web session (if created) is provider-side;
                    // stop the local process either way.
                    let _ = child.kill().await;
                    break session.is_some();
                }
            }
        };

        // Drain whatever landed after the loop ended, then settle the child.
        while let Ok(line) = line_rx.try_recv() {
            all_output.push_str(&line);
            all_output.push('\n');
        }
        let exit_ok = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => status.success() || exited,
            _ => exited,
        };

        if session.is_none() {
            session = parse_session_ref(&all_output);
        }
        match session {
            Some((session_id, url)) => Ok(CloudTaskRef {
                agent: AgentKind::ClaudeCode,
                task_id: session_id,
                url,
                env_id: None,
            }),
            None if exit_ok => Err(CloudError::Parse(format!(
                "claude --cloud finished but no session id was found in output: {}",
                all_output.trim().chars().take(300).collect::<String>()
            ))),
            None => Err(launch_failure(&all_output)),
        }
    }

    async fn poll(&self, _task: &CloudTaskRef) -> Result<CloudPollStatus, CloudError> {
        // No headless status surface; the orchestrator watches the branch.
        Ok(CloudPollStatus::Unknown)
    }

    async fn fetch_results(
        &self,
        _task: &CloudTaskRef,
        _worktree: &Path,
    ) -> Result<String, CloudError> {
        // Results arrive as branch commits, fetched by the caller via am-vcs.
        Ok(String::new())
    }
}

enum ReaderKind {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

async fn forward_lines<R>(stream: R, tx: tokio::sync::mpsc::UnboundedSender<String>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// Pull a web-session reference out of launch output. Session ids appear as
/// `session_<hash>` (transcript URLs) or `cse_<hash>` (the env-var form); URLs
/// look like `https://claude.ai/code/session_<hash>`.
fn parse_session_ref(text: &str) -> Option<(Option<String>, Option<String>)> {
    let url = extract_url(text).filter(|u| u.contains("claude.ai"));
    let id = find_session_token(text);
    if url.is_none() && id.is_none() {
        return None;
    }
    let id = id.or_else(|| {
        url.as_deref()
            .and_then(|u| find_session_token(u.rsplit('/').next().unwrap_or("")))
    });
    Some((id, url))
}

fn find_session_token(text: &str) -> Option<String> {
    for raw in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let is_session = raw.strip_prefix("session_").is_some_and(|r| r.len() >= 8);
        let is_cse = raw.strip_prefix("cse_").is_some_and(|r| r.len() >= 8);
        if is_session || is_cse {
            return Some(raw.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_url_and_id() {
        let out = "Creating session...\nView at https://claude.ai/code/session_01AbCdEf1234\n";
        let (id, url) = parse_session_ref(out).expect("parsed");
        assert_eq!(id.as_deref(), Some("session_01AbCdEf1234"));
        assert_eq!(
            url.as_deref(),
            Some("https://claude.ai/code/session_01AbCdEf1234")
        );
    }

    #[test]
    fn parses_bare_cse_id() {
        let (id, url) = parse_session_ref("session created: cse_9f8e7d6c5b").expect("parsed");
        assert_eq!(id.as_deref(), Some("cse_9f8e7d6c5b"));
        assert!(url.is_none());
    }

    #[test]
    fn ignores_unrelated_output() {
        assert!(parse_session_ref("cloning repository...").is_none());
        // Short tokens that merely share the prefix should not match.
        assert!(parse_session_ref("session_id field").is_none());
    }
}
