//! Codex Cloud client over the `codex cloud` CLI surface.
//!
//! Verified against codex-cli 0.142.0 from the ChatGPT VS Code extension
//! (2026-07-03):
//! - `codex cloud exec --env <ENV_ID> [--branch <BRANCH>] [QUERY]` submits a
//!   task and prints a task URL; non-zero exit on submission failure.
//! - `codex cloud list --json [--env ID] [--limit 1-20] [--cursor C]` emits
//!   `{"tasks": [{id, url, title, status, updated_at, environment_id, ...}],
//!   "cursor": ...}`.
//! - `codex cloud status <TASK_ID>` / `codex cloud diff <TASK_ID>` exist but
//!   are undocumented; `codex apply <TASK_ID>` git-applies the latest diff.

use std::path::Path;
use std::time::Duration;

use am_proto::{AgentKind, CloudAvailability};
use async_trait::async_trait;
use serde_json::Value;

use super::{
    extract_url, launch_failure, run_cloud_command, CloudError, CloudLaunchRequest,
    CloudPollStatus, CloudTaskClient, CloudTaskRef,
};
use crate::detect::find_binary;

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_TIMEOUT: Duration = Duration::from_secs(60);
const APPLY_TIMEOUT: Duration = Duration::from_secs(300);

pub struct CodexCloudClient;

#[async_trait]
impl CloudTaskClient for CodexCloudClient {
    fn agent(&self) -> AgentKind {
        AgentKind::Codex
    }

    async fn availability(&self, codex_env_id: Option<&str>) -> CloudAvailability {
        let mut blockers = Vec::new();
        let mut authenticated = false;

        let binary = find_binary("codex");
        match &binary {
            None => blockers.push("Codex CLI is not installed".to_string()),
            Some(bin) => {
                match run_cloud_command(bin, &["login", "status"], None, POLL_TIMEOUT).await {
                    Ok(out) if out.status_success => {
                        // The status line lands on stderr; check both streams
                        // and guard against "Not logged in".
                        let combined = out.combined().to_lowercase();
                        authenticated =
                            combined.contains("logged in") && !combined.contains("not logged in");
                        if !authenticated {
                            blockers.push("Codex CLI is not signed in".to_string());
                        }
                    }
                    _ => blockers.push("Codex CLI is not signed in".to_string()),
                }
            }
        }

        if codex_env_id.is_none_or(|id| id.trim().is_empty()) {
            blockers.push(
                "No Codex Cloud environment id configured (create one at chatgpt.com/codex, then set it in Settings → Cloud Continuity)"
                    .to_string(),
            );
        }

        // A cheap end-to-end probe: listing tasks exercises cloud auth and org
        // enablement without creating anything.
        if authenticated && blockers.is_empty() {
            if let Some(bin) = &binary {
                match run_cloud_command(
                    bin,
                    &["cloud", "list", "--limit", "1", "--json"],
                    None,
                    POLL_TIMEOUT,
                )
                .await
                {
                    Ok(out) if out.status_success => {}
                    Ok(out) => blockers.push(format!(
                        "Codex Cloud is unreachable: {}",
                        out.combined().trim().chars().take(200).collect::<String>()
                    )),
                    Err(e) => blockers.push(format!("Codex Cloud is unreachable: {e}")),
                }
            }
        }

        CloudAvailability {
            agent: AgentKind::Codex,
            ready: blockers.is_empty(),
            authenticated,
            blockers,
            checked_at: am_proto::now(),
        }
    }

    async fn launch(&self, req: &CloudLaunchRequest) -> Result<CloudTaskRef, CloudError> {
        let binary = find_binary("codex").ok_or(CloudError::NotInstalled("codex"))?;
        let env_id = req
            .env_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                CloudError::Launch("no Codex Cloud environment id configured".to_string())
            })?;

        let mut args: Vec<&str> = vec!["cloud", "exec", "--env", env_id];
        if let Some(branch) = req.branch.as_deref() {
            args.push("--branch");
            args.push(branch);
        }
        args.push(&req.prompt);

        let out = run_cloud_command(&binary, &args, Some(&req.worktree), LAUNCH_TIMEOUT).await?;
        if !out.status_success {
            return Err(launch_failure(&out.combined()));
        }

        let combined = out.combined();
        let url = extract_url(&combined);
        let task_id = url.as_deref().and_then(task_id_from_url);

        // The exec output shape isn't contractual; if no id was parseable,
        // resolve the newest task for this environment from the list API.
        let (task_id, url) = if task_id.is_some() {
            (task_id, url)
        } else {
            match newest_task(&binary, env_id).await {
                Ok(Some((id, task_url))) => (Some(id), url.or(task_url)),
                _ => (None, url),
            }
        };

        if task_id.is_none() && url.is_none() {
            return Err(CloudError::Parse(format!(
                "launch succeeded but no task id or URL was found in output: {}",
                combined.trim().chars().take(300).collect::<String>()
            )));
        }

        Ok(CloudTaskRef {
            agent: AgentKind::Codex,
            task_id,
            url,
            env_id: Some(env_id.to_string()),
        })
    }

    async fn poll(&self, task: &CloudTaskRef) -> Result<CloudPollStatus, CloudError> {
        let binary = find_binary("codex").ok_or(CloudError::NotInstalled("codex"))?;
        let Some(task_id) = task.task_id.as_deref() else {
            return Ok(CloudPollStatus::Unknown);
        };

        let mut args: Vec<&str> = vec!["cloud", "list", "--json", "--limit", "20"];
        if let Some(env) = task.env_id.as_deref() {
            args.push("--env");
            args.push(env);
        }
        let out = run_cloud_command(&binary, &args, None, POLL_TIMEOUT).await?;
        if !out.status_success {
            return Err(CloudError::Command(
                out.combined().trim().chars().take(300).collect(),
            ));
        }

        let parsed: Value = serde_json::from_str(out.stdout.trim())
            .map_err(|e| CloudError::Parse(format!("cloud list JSON: {e}")))?;
        let Some(tasks) = parsed.get("tasks").and_then(Value::as_array) else {
            return Err(CloudError::Parse(
                "cloud list JSON has no tasks array".into(),
            ));
        };

        let found = tasks
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(task_id));
        match found {
            Some(t) => {
                let status = t.get("status").and_then(Value::as_str).unwrap_or("");
                Ok(map_status(status))
            }
            // Recent-20 miss: fall back to the (undocumented) status command
            // before giving up, so long-lived runs on busy accounts still poll.
            None => poll_via_status(&binary, task_id).await,
        }
    }

    async fn fetch_results(
        &self,
        task: &CloudTaskRef,
        worktree: &Path,
    ) -> Result<String, CloudError> {
        let binary = find_binary("codex").ok_or(CloudError::NotInstalled("codex"))?;
        let Some(task_id) = task.task_id.as_deref() else {
            return Err(CloudError::Command(
                "no provider task id recorded for this cloud run".into(),
            ));
        };

        let out =
            run_cloud_command(&binary, &["apply", task_id], Some(worktree), APPLY_TIMEOUT).await?;
        if out.status_success {
            let summary = out.combined().trim().chars().take(600).collect::<String>();
            return Ok(if summary.is_empty() {
                "applied Codex Cloud diff".to_string()
            } else {
                summary
            });
        }

        // `apply` fails when the tree drifted or the task made no changes.
        // Grab the diff so the failure record still shows what the task did.
        let diff = run_cloud_command(
            &binary,
            &["cloud", "diff", task_id],
            Some(worktree),
            POLL_TIMEOUT,
        )
        .await
        .map(|d| d.stdout)
        .unwrap_or_default();
        if diff.trim().is_empty() {
            return Ok("Codex Cloud task produced no diff".to_string());
        }
        Err(CloudError::Command(format!(
            "codex apply failed: {}; diff preserved on the provider (task {task_id})",
            out.combined().trim().chars().take(300).collect::<String>()
        )))
    }
}

/// Task ids live in URLs like `https://chatgpt.com/codex/tasks/task_e_abc123`.
fn task_id_from_url(url: &str) -> Option<String> {
    let last = url.trim_end_matches('/').rsplit('/').next()?;
    let id = last.split(['?', '#']).next().unwrap_or(last);
    if id.starts_with("task") && id.len() > 5 {
        Some(id.to_string())
    } else {
        None
    }
}

async fn newest_task(
    binary: &Path,
    env_id: &str,
) -> Result<Option<(String, Option<String>)>, CloudError> {
    let out = run_cloud_command(
        binary,
        &["cloud", "list", "--json", "--limit", "1", "--env", env_id],
        None,
        POLL_TIMEOUT,
    )
    .await?;
    if !out.status_success {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(out.stdout.trim())
        .map_err(|e| CloudError::Parse(format!("cloud list JSON: {e}")))?;
    let task = parsed
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|t| t.first());
    Ok(task.and_then(|t| {
        let id = t.get("id").and_then(Value::as_str)?.to_string();
        let url = t.get("url").and_then(Value::as_str).map(str::to_string);
        Some((id, url))
    }))
}

async fn poll_via_status(binary: &Path, task_id: &str) -> Result<CloudPollStatus, CloudError> {
    let out = run_cloud_command(binary, &["cloud", "status", task_id], None, POLL_TIMEOUT).await?;
    if !out.status_success {
        return Ok(CloudPollStatus::Unknown);
    }
    Ok(map_status(&out.combined()))
}

/// Map provider status strings (from JSON or plain text) onto poll states.
/// Codex statuses aren't contractual, so match generously.
fn map_status(raw: &str) -> CloudPollStatus {
    let s = raw.to_lowercase();
    if s.contains("pending") || s.contains("queued") || s.contains("provision") {
        CloudPollStatus::Provisioning
    } else if s.contains("in_progress") || s.contains("in progress") || s.contains("running") {
        CloudPollStatus::Running
    } else if s.contains("completed") || s.contains("succeeded") || s.contains("ready") {
        CloudPollStatus::Completed
    } else if s.contains("expired") {
        CloudPollStatus::Expired
    } else if s.contains("failed") || s.contains("error") || s.contains("cancelled") {
        CloudPollStatus::Failed(raw.trim().chars().take(200).collect())
    } else {
        CloudPollStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_id_from_url() {
        assert_eq!(
            task_id_from_url("https://chatgpt.com/codex/tasks/task_e_abc123").as_deref(),
            Some("task_e_abc123")
        );
        assert_eq!(
            task_id_from_url("https://chatgpt.com/codex/tasks/task_e_abc123?x=1").as_deref(),
            Some("task_e_abc123")
        );
        assert_eq!(task_id_from_url("https://chatgpt.com/codex"), None);
    }

    #[test]
    fn maps_status_strings() {
        assert_eq!(map_status("in_progress"), CloudPollStatus::Running);
        assert_eq!(map_status("Status: Completed"), CloudPollStatus::Completed);
        assert_eq!(map_status("queued"), CloudPollStatus::Provisioning);
        assert!(matches!(
            map_status("failed: boom"),
            CloudPollStatus::Failed(_)
        ));
        assert_eq!(map_status("weird"), CloudPollStatus::Unknown);
    }
}
