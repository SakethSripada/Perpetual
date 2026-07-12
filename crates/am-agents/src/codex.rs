//! Codex CLI adapter.
//!
//! Drives `codex exec --json` in the task worktree using the user's existing
//! Codex/ChatGPT login. The Codex exec JSONL stream is already normalized by the
//! CLI into thread/item lifecycle events; this adapter maps those events into
//! Perpetual's provider-independent [`NormalizedEvent`] model.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::detect::{binary_version, find_binary};
use crate::limits::detect_usage_limit;
use crate::network::detect_network_error;
use crate::process::ManagedChild;
use crate::runtime::{
    buffered_diagnostics, push_diagnostic_line, spawn_for_runtime_with_env, with_diagnostics,
    RuntimeLimits,
};
use crate::{
    AgentAdapter, AgentError, AgentInstallStatus, AgentKind, ApprovalDecision, ApprovalResponder,
    ChangeKind, LocalModelRuntime, NormalizedEvent, PermissionPolicy, SessionControl,
    SessionHandle, SessionRef, SessionSpec, SessionStatus,
};

const BIN: &str = "codex";
const CHANNEL_CAPACITY: usize = 256;
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }

    async fn launch(
        &self,
        spec: SessionSpec,
        resume: Option<SessionRef>,
    ) -> Result<SessionHandle, AgentError> {
        // Host runs use app-server for both approvals and structured user-input
        // requests. In read-only/plan mode the fallback responder can only deny
        // an unexpected action approval, while request_user_input still reaches
        // the workbench as a first-class question.
        if matches!(spec.runtime, crate::SessionRuntime::Host { .. }) {
            let approver = spec.approver.clone().unwrap_or_else(|| {
                ApprovalResponder::new(|_| Box::pin(async { ApprovalDecision::Deny }))
            });
            match crate::codex_app_server::launch(spec.clone(), resume.clone(), approver).await {
                Ok(handle) => return Ok(handle),
                Err(err) => {
                    tracing::warn!(
                        error = %err.into_message(),
                        "codex app-server interactive transport failed; falling back to exec"
                    );
                }
            }
        }

        let args = build_args(&spec, resume.as_ref());
        let envs = session_env(&spec);
        tracing::debug!(?args, worktree = ?spec.worktree, "launching codex");

        let mut child =
            spawn_for_runtime_with_env(BIN, "codex", &args, &spec.worktree, &spec.runtime, &envs)
                .await?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| AgentError::Spawn("no stdout pipe".into()))?;
        let stderr = child.take_stderr();

        let (tx, rx) = mpsc::channel::<NormalizedEvent>(CHANNEL_CAPACITY);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        let limits = spec.runtime.limits();
        tokio::spawn(drive(child, stdout, stderr, tx, cancel_rx, limits));

        Ok(SessionHandle {
            events: rx,
            control: SessionControl::new(cancel_tx),
        })
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    async fn detect(&self) -> AgentInstallStatus {
        tokio::task::spawn_blocking(|| {
            let binary = find_binary(BIN);
            let version = binary.as_ref().and_then(|b| binary_version(b));
            let authenticated = binary
                .as_ref()
                .map(|b| codex_authenticated(b))
                .unwrap_or(false);
            AgentInstallStatus {
                kind: AgentKind::Codex,
                installed: binary.is_some(),
                authenticated,
                version,
                binary_path: binary,
            }
        })
        .await
        .unwrap_or(AgentInstallStatus {
            kind: AgentKind::Codex,
            installed: false,
            authenticated: false,
            version: None,
            binary_path: None,
        })
    }

    async fn start(&self, spec: SessionSpec) -> Result<SessionHandle, AgentError> {
        self.launch(spec, None).await
    }

    async fn resume(
        &self,
        prior: SessionRef,
        spec: SessionSpec,
    ) -> Result<SessionHandle, AgentError> {
        self.launch(spec, Some(prior)).await
    }
}

fn codex_authenticated(binary: &Path) -> bool {
    let output = Command::new(binary)
        .args(["login", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // The CLI prints "Logged in using ChatGPT" to stderr, not stdout, so we
        // must capture (not discard) stderr to read the real status.
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) if output.status.success() => login_status_authenticated(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        _ => false,
    }
}

/// Classify `codex login status` output. The CLI writes its status line to
/// stderr, so both streams are inspected. Guard against "Not logged in", which
/// also contains the "logged in" substring.
fn login_status_authenticated(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_lowercase();
    combined.contains("logged in") && !combined.contains("not logged in")
}

/// Build the `codex exec` argument vector. Every value is a discrete argument;
/// the prompt is never interpolated into a shell string.
fn build_args(spec: &SessionSpec, resume: Option<&SessionRef>) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--skip-git-repo-check".to_string(),
    ];

    match spec.permission {
        PermissionPolicy::ReadOnly => {
            args.push("--sandbox".into());
            args.push("read-only".into());
        }
        PermissionPolicy::WorkspaceWrite => {
            if matches!(spec.runtime, crate::SessionRuntime::DockerSandbox { .. }) {
                args.push("--dangerously-bypass-approvals-and-sandbox".into());
            } else {
                args.push("--sandbox".into());
                args.push("workspace-write".into());
            }
        }
        PermissionPolicy::Ask => {
            // Live approval runs over the app-server transport (see `launch`).
            // This exec arm is only reached as a fallback (no approver wired);
            // run sandboxed like workspace-write.
            args.push("--sandbox".into());
            args.push("workspace-write".into());
        }
        PermissionPolicy::Autonomous => {
            args.push("--dangerously-bypass-approvals-and-sandbox".into());
        }
    }

    if let Some(local) = spec.local_model.as_ref() {
        push_local_model_args(&mut args, local);
    } else if let Some(model) = normalize_model(spec.model.as_deref()) {
        args.push("--model".into());
        args.push(model);
    }

    if let Some(effort) = normalize_reasoning(spec.reasoning.as_deref()) {
        args.push("-c".into());
        args.push(format!("model_reasoning_effort=\"{effort}\""));
    }

    if let Some(policy) = spec.policy.as_ref() {
        push_policy_args(&mut args, policy);
    }

    if let Some(prior) = resume {
        args.push("resume".into());
        args.push(prior.agent_session_id.clone());
        args.push(spec.prompt.clone());
    } else {
        args.push(spec.prompt.clone());
    }

    args
}

fn push_policy_args(args: &mut Vec<String>, policy: &crate::AgentPolicyRuntime) {
    if !policy.allowed_mcp_servers.is_empty() {
        for server in &policy.allowed_mcp_servers {
            if server == "perpetual" {
                continue;
            }
            args.push("-c".into());
            args.push(format!("mcp_servers.{server}.enabled=true"));
        }
    }
    for server in &policy.denied_mcp_servers {
        if server == "*" {
            continue;
        }
        args.push("-c".into());
        args.push(format!("mcp_servers.{server}.enabled=false"));
    }
    for tool in &policy.allowed_tools {
        if let Some((server, tool_name)) = parse_mcp_tool(tool) {
            args.push("-c".into());
            args.push(format!(
                "mcp_servers.{server}.enabled_tools=[{}]",
                toml_string(tool_name)
            ));
        }
    }
    for tool in &policy.denied_tools {
        if let Some((server, tool_name)) = parse_mcp_tool(tool) {
            args.push("-c".into());
            args.push(format!(
                "mcp_servers.{server}.disabled_tools=[{}]",
                toml_string(tool_name)
            ));
        }
    }
    if !policy.env_allowlist.is_empty() {
        args.push("-c".into());
        let values = policy
            .env_allowlist
            .iter()
            .map(|value| toml_string(value))
            .collect::<Vec<_>>()
            .join(",");
        args.push(format!("shell_environment_policy.include_only=[{values}]"));
    }
    if !policy.denied_context_globs.is_empty() {
        args.push("-c".into());
        let entries = policy
            .denied_context_globs
            .iter()
            .map(|glob| format!("{}=\"deny\"", toml_string(glob)))
            .collect::<Vec<_>>()
            .join(",");
        args.push(format!(
            "permissions.perpetual_policy.filesystem.\":workspace_roots\"={{{entries}}}"
        ));
    }
}

fn parse_mcp_tool(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() || server.contains('*') {
        return None;
    }
    Some((server, tool))
}

fn normalize_model(model: Option<&str>) -> Option<String> {
    let value = clean_override(model)?;
    let lower = value.to_ascii_lowercase();
    if matches!(lower.as_str(), "opus" | "sonnet" | "haiku" | "fable")
        || lower.starts_with("claude-")
    {
        return None;
    }
    Some(value.to_string())
}

const LOCAL_MODEL_TOKEN_ENV: &str = "PERPETUAL_LOCAL_MODEL_TOKEN";
const LOCAL_PROVIDER_ID: &str = "perpetual_local";

fn push_local_model_args(args: &mut Vec<String>, local: &LocalModelRuntime) {
    if uses_builtin_local_provider(local) {
        args.push("--oss".into());
        args.push("-c".into());
        args.push(format!(
            "oss_provider={}",
            toml_string(local.provider.codex_oss_provider())
        ));
    } else {
        args.push("-c".into());
        args.push(format!("model_provider={}", toml_string(LOCAL_PROVIDER_ID)));
        args.push("-c".into());
        args.push(format!(
            "model_providers.{LOCAL_PROVIDER_ID}.name={}",
            toml_string("Perpetual Local")
        ));
        args.push("-c".into());
        args.push(format!(
            "model_providers.{LOCAL_PROVIDER_ID}.base_url={}",
            toml_string(&local_openai_base_url(local))
        ));
        if local
            .api_token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty())
        {
            args.push("-c".into());
            args.push(format!(
                "model_providers.{LOCAL_PROVIDER_ID}.env_key={}",
                toml_string(LOCAL_MODEL_TOKEN_ENV)
            ));
        }
    }

    args.push("--model".into());
    args.push(local.model.trim().to_string());
}

fn uses_builtin_local_provider(local: &LocalModelRuntime) -> bool {
    local
        .api_token
        .as_ref()
        .is_none_or(|token| token.trim().is_empty())
        && local
            .base_url
            .as_deref()
            .map(|base_url| normalize_base_url(base_url) == local.provider.default_base_url())
            .unwrap_or(true)
}

fn local_openai_base_url(local: &LocalModelRuntime) -> String {
    let base = normalize_base_url(
        local
            .base_url
            .as_deref()
            .unwrap_or_else(|| local.provider.default_base_url()),
    );
    if base.ends_with("/v1") {
        base
    } else {
        format!("{base}/v1")
    }
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn local_model_env(spec: &SessionSpec) -> Vec<(String, String)> {
    spec.local_model
        .as_ref()
        .and_then(|local| local.api_token.as_deref())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| vec![(LOCAL_MODEL_TOKEN_ENV.to_string(), token.to_string())])
        .unwrap_or_default()
}

pub(crate) fn session_env(spec: &SessionSpec) -> Vec<(String, String)> {
    let mut envs = local_model_env(spec);
    if let Some(policy) = spec.policy.as_ref() {
        if !policy.env_allowlist.is_empty() {
            envs.push((
                "PERPETUAL_POLICY_ENV_ALLOWLIST".into(),
                policy.env_allowlist.join(","),
            ));
        }
    }
    envs
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn normalize_reasoning(reasoning: Option<&str>) -> Option<String> {
    // The installed CLI owns validation. Keeping a hard-coded allow-list here
    // silently dropped new catalog values such as `max` and `ultra`.
    Some(clean_override(reasoning)?.to_ascii_lowercase())
}

fn clean_override(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() || matches!(value, "default" | "auto") {
        None
    } else {
        Some(value)
    }
}

async fn drive(
    mut child: ManagedChild,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    tx: mpsc::Sender<NormalizedEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
    limits: RuntimeLimits,
) {
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_task = stderr.map(|se| {
        let buf = stderr_buf.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(se).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                push_diagnostic_line(&buf, &line);
            }
        })
    });

    let stdout_buf = Arc::new(Mutex::new(String::new()));
    let mut reader = BufReader::new(stdout).lines();
    let mut cancelled = false;
    let mut timeout_message: Option<String> = None;
    let mut codex_terminal_status: Option<SessionStatus> = None;
    let mut saw_structured_output = false;
    let hard_timeout = tokio::time::sleep(limits.run_timeout);
    let idle_timeout = tokio::time::sleep(limits.idle_timeout);
    let startup_timeout = tokio::time::sleep(limits.startup_timeout);
    tokio::pin!(hard_timeout);
    tokio::pin!(idle_timeout);
    tokio::pin!(startup_timeout);

    loop {
        tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() { continue; }
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(value) => {
                            saw_structured_output = true;
                            idle_timeout.as_mut().reset(tokio::time::Instant::now() + limits.idle_timeout);
                            let parsed = parse_line(&value);
                            if parsed.terminal.is_some() {
                                codex_terminal_status = parsed.terminal;
                            }
                            for event in parsed.events {
                                if tx.send(event).await.is_err() {
                                    cancelled = true;
                                    break;
                                }
                            }
                            if cancelled { break; }
                        }
                        Err(_) => {
                            push_diagnostic_line(&stdout_buf, trimmed);
                            tracing::debug!(line = %trimmed, "ignoring non-json stream line");
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => { tracing::warn!(error = %e, "stdout read error"); break; }
            },
            _ = &mut cancel_rx => { cancelled = true; break; }
            _ = &mut hard_timeout => {
                timeout_message = Some("agent run timed out".to_string());
                cancelled = true;
                break;
            }
            _ = &mut idle_timeout => {
                timeout_message = Some("agent produced no structured output before the idle timeout".to_string());
                cancelled = true;
                break;
            }
            _ = &mut startup_timeout, if !saw_structured_output => {
                timeout_message = Some("agent did not produce structured output before the startup timeout".to_string());
                cancelled = true;
                break;
            }
        }
    }

    if cancelled {
        child.terminate_group();
        if tokio::time::timeout(limits.stop_grace.max(TERMINATE_GRACE), child.wait())
            .await
            .is_err()
        {
            child.kill_group();
        }
    }

    let exit = child.wait().await;
    let success = matches!(&exit, Ok(status) if status.success());

    let final_status = if cancelled {
        SessionStatus::Interrupted
    } else if let Some(status) = codex_terminal_status {
        status
    } else if success {
        SessionStatus::Completed
    } else {
        SessionStatus::Failed
    };

    if let Some(message) = timeout_message {
        let diagnostics = buffered_diagnostics(&stdout_buf, &stderr_buf);
        let _ = tx
            .send(NormalizedEvent::Error {
                message: with_diagnostics(&message, &diagnostics),
                retryable: true,
            })
            .await;
    }

    if !cancelled && !success && codex_terminal_status.is_none() {
        let err = buffered_diagnostics(&stdout_buf, &stderr_buf);
        let err = err.trim();
        if !err.is_empty() {
            match detect_usage_limit(err) {
                Some(reset_at) => {
                    let _ = tx
                        .send(NormalizedEvent::UsageLimitReached { reset_at })
                        .await;
                }
                None if let Some(message) = detect_network_error(err) => {
                    let _ = tx
                        .send(NormalizedEvent::NetworkUnavailable { message })
                        .await;
                }
                None => {
                    let _ = tx
                        .send(NormalizedEvent::Error {
                            message: truncate(err, 2000),
                            retryable: false,
                        })
                        .await;
                }
            }
        }
    }

    let _ = tx
        .send(NormalizedEvent::SessionEnded {
            status: final_status,
        })
        .await;

    if let Some(task) = stderr_task {
        task.abort();
    }
}

#[derive(Debug, Default)]
pub(crate) struct ParsedCodexLine {
    pub events: Vec<NormalizedEvent>,
    pub terminal: Option<SessionStatus>,
}

/// Parse one `codex exec --json` line. The driver emits the final
/// `SessionEnded`; this parser only returns a terminal-status hint.
pub(crate) fn parse_line(v: &Value) -> ParsedCodexLine {
    let mut parsed = ParsedCodexLine::default();
    match v.get("type").and_then(|t| t.as_str()) {
        Some("thread.started") => {
            if let Some(thread_id) = v.get("thread_id").and_then(|s| s.as_str()) {
                parsed.events.push(NormalizedEvent::SessionStarted {
                    session_id: thread_id.to_string(),
                });
            }
        }
        Some("turn.started") => {}
        Some("turn.completed") => {
            if let Some(usage) = parse_usage(v.get("usage")) {
                parsed.events.push(usage);
            }
            parsed.terminal = Some(SessionStatus::Completed);
        }
        Some("turn.failed") => {
            if let Some(message) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                push_error_or_limit(&mut parsed.events, message);
            }
            parsed.terminal = Some(SessionStatus::Failed);
        }
        Some("error") => {
            if let Some(message) = v.get("message").and_then(|m| m.as_str()) {
                push_error_or_limit(&mut parsed.events, message);
            }
        }
        Some("item.started") => {
            if let Some(item) = v.get("item") {
                parse_item(item, ItemPhase::Started, &mut parsed.events);
            }
        }
        Some("item.updated") => {
            if let Some(item) = v.get("item") {
                parse_item(item, ItemPhase::Updated, &mut parsed.events);
            }
        }
        Some("item.completed") => {
            if let Some(item) = v.get("item") {
                parse_item(item, ItemPhase::Completed, &mut parsed.events);
            }
        }
        _ => {}
    }
    parsed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemPhase {
    Started,
    Updated,
    Completed,
}

fn parse_item(item: &Value, phase: ItemPhase, out: &mut Vec<NormalizedEvent>) {
    match item.get("type").and_then(|t| t.as_str()) {
        Some("agent_message") if phase == ItemPhase::Completed => {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                if !text.trim().is_empty() {
                    out.push(NormalizedEvent::AssistantText {
                        text: text.to_string(),
                    });
                }
            }
        }
        Some("command_execution") => parse_command_execution(item, phase, out),
        Some("file_change") if phase == ItemPhase::Completed => {
            parse_file_change(item, out);
        }
        Some("mcp_tool_call") => parse_mcp_tool_call(item, phase, out),
        Some("web_search") => parse_web_search(item, phase, out),
        Some("error") if phase == ItemPhase::Completed => {
            if let Some(message) = item.get("message").and_then(|m| m.as_str()) {
                push_error_or_limit(out, message);
            }
        }
        _ => {}
    }
}

fn parse_command_execution(item: &Value, phase: ItemPhase, out: &mut Vec<NormalizedEvent>) {
    let command = item
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    match phase {
        ItemPhase::Started => out.push(NormalizedEvent::ToolUse {
            name: "Command".to_string(),
            input: json!({ "command": command }),
        }),
        ItemPhase::Completed => {
            let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let exit_code = item.get("exit_code").and_then(|c| c.as_i64());
            let output = item
                .get("aggregated_output")
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let summary = if !output.trim().is_empty() {
                truncate(output, 800)
            } else if let Some(code) = exit_code {
                format!("exit code {code}")
            } else {
                status.to_string()
            };
            out.push(NormalizedEvent::ToolResult {
                ok: status == "completed" && exit_code.unwrap_or(0) == 0,
                summary,
            });
        }
        ItemPhase::Updated => {}
    }
}

fn parse_file_change(item: &Value, out: &mut Vec<NormalizedEvent>) {
    let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let ok = status == "completed";

    if let Some(changes) = item.get("changes").and_then(|c| c.as_array()) {
        for change in changes {
            let Some(path) = change.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            let kind = match change.get("kind").and_then(|k| k.as_str()) {
                Some("add") => ChangeKind::Created,
                Some("delete") => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            out.push(NormalizedEvent::FileChanged {
                path: path.into(),
                change: kind,
            });
        }

        out.push(NormalizedEvent::ToolResult {
            ok,
            summary: format!(
                "{} file change{}",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            ),
        });
    }
}

fn parse_mcp_tool_call(item: &Value, phase: ItemPhase, out: &mut Vec<NormalizedEvent>) {
    let server = item.get("server").and_then(|s| s.as_str()).unwrap_or("");
    let tool = item.get("tool").and_then(|t| t.as_str()).unwrap_or("tool");
    let name = if server.is_empty() {
        tool.to_string()
    } else {
        format!("{server}/{tool}")
    };

    match phase {
        ItemPhase::Started => out.push(NormalizedEvent::ToolUse {
            name,
            input: json!({
                "server": server,
                "tool": tool,
                "arguments": item.get("arguments").cloned().unwrap_or(Value::Null),
            }),
        }),
        ItemPhase::Completed => {
            let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let ok = status == "completed" && item.get("error").is_none();
            let summary = item
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .or_else(|| item.get("result").map(|r| truncate(&r.to_string(), 800)))
                .unwrap_or_else(|| status.to_string());
            out.push(NormalizedEvent::ToolResult { ok, summary });
        }
        ItemPhase::Updated => {}
    }
}

fn parse_web_search(item: &Value, phase: ItemPhase, out: &mut Vec<NormalizedEvent>) {
    let query = item.get("query").and_then(|q| q.as_str()).unwrap_or("");
    match phase {
        ItemPhase::Started => out.push(NormalizedEvent::ToolUse {
            name: "WebSearch".to_string(),
            input: json!({
                "query": query,
                "action": item.get("action").cloned().unwrap_or(Value::Null),
            }),
        }),
        ItemPhase::Completed => out.push(NormalizedEvent::ToolResult {
            ok: true,
            summary: truncate(query, 800),
        }),
        ItemPhase::Updated => {}
    }
}

fn parse_usage(usage: Option<&Value>) -> Option<NormalizedEvent> {
    let usage = usage?;
    let input = usage
        .get("input_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    let output = usage
        .get("output_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    if input + output > 0 {
        Some(NormalizedEvent::TokenUsage { input, output })
    } else {
        None
    }
}

pub(crate) fn push_error_or_limit(out: &mut Vec<NormalizedEvent>, message: &str) {
    match detect_usage_limit(message) {
        Some(reset_at) => out.push(NormalizedEvent::UsageLimitReached { reset_at }),
        None if let Some(message) = detect_network_error(message) => {
            out.push(NormalizedEvent::NetworkUnavailable { message });
        }
        None if !message.trim().is_empty() => out.push(NormalizedEvent::Error {
            message: truncate(message, 2000),
            retryable: false,
        }),
        None => {}
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_start_args_with_workspace_write() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: Some("gpt-5.1-codex".into()),
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        assert_eq!(
            build_args(&spec, None),
            vec![
                "exec",
                "--json",
                "--color",
                "never",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "--model",
                "gpt-5.1-codex",
                "Implement it",
            ]
        );
    }

    #[test]
    fn builds_args_with_reasoning_effort() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: None,
            reasoning: Some("high".into()),
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        let pos = args
            .iter()
            .position(|a| a == "-c")
            .expect("-c flag present");
        assert_eq!(args[pos + 1], "model_reasoning_effort=\"high\"");
    }

    #[test]
    fn docker_workspace_write_bypasses_internal_codex_approvals() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::DockerSandbox {
                name: "perpetual-test".into(),
                cpus: 2,
                memory: "4g".into(),
                network_preset: "balanced".into(),
                limits: crate::RuntimeLimits::default(),
            },
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"));
        assert!(!args.iter().any(|arg| arg == "workspace-write"));
    }

    #[test]
    fn docker_read_only_keeps_read_only_sandbox() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Explain it".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::ReadOnly,
            runtime: crate::SessionRuntime::DockerSandbox {
                name: "perpetual-test".into(),
                cpus: 2,
                memory: "4g".into(),
                network_preset: "balanced".into(),
                limits: crate::RuntimeLimits::default(),
            },
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(!args
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"));
    }

    #[test]
    fn drops_claude_model_alias_but_forwards_discovered_effort_for_codex() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Continue".into(),
            model: Some("opus".into()),
            reasoning: Some("max".into()),
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(!args.iter().any(|arg| arg == "--model" || arg == "opus"));
        assert!(args
            .iter()
            .any(|arg| arg == "model_reasoning_effort=\"max\""));
    }

    #[test]
    fn drops_full_claude_model_id_for_codex() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Continue".into(),
            model: Some("claude-opus-4-8".into()),
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(!args
            .iter()
            .any(|arg| arg == "--model" || arg == "claude-opus-4-8"));
    }

    #[test]
    fn accepts_codex_reasoning_efforts() {
        let mut seen = Vec::new();
        for effort in ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"] {
            let spec = SessionSpec {
                worktree: "/tmp/worktree".into(),
                prompt: "Implement it".into(),
                model: None,
                reasoning: Some(effort.into()),
                local_model: None,
                permission: PermissionPolicy::WorkspaceWrite,
                runtime: crate::SessionRuntime::default(),
                policy: None,
                approver: None,
            };
            let args = build_args(&spec, None);
            let pos = args
                .iter()
                .position(|a| a == "-c")
                .expect("-c flag present");
            seen.push(args[pos + 1].clone());
        }

        assert_eq!(
            seen,
            vec![
                "model_reasoning_effort=\"minimal\"",
                "model_reasoning_effort=\"low\"",
                "model_reasoning_effort=\"medium\"",
                "model_reasoning_effort=\"high\"",
                "model_reasoning_effort=\"xhigh\"",
                "model_reasoning_effort=\"max\"",
                "model_reasoning_effort=\"ultra\"",
            ]
        );
    }

    #[test]
    fn builds_builtin_ollama_oss_args() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: Some("gpt-5.5".into()),
            reasoning: None,
            local_model: Some(LocalModelRuntime {
                provider: am_proto::LocalModelProviderKind::Ollama,
                model: "qwen3:8b".into(),
                base_url: None,
                api_token: None,
            }),
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args.iter().any(|arg| arg == "--oss"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "oss_provider=\"ollama\""]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "qwen3:8b"]));
        assert!(!args.iter().any(|arg| arg == "gpt-5.5"));
    }

    #[test]
    fn builds_builtin_lm_studio_oss_args() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: None,
            reasoning: None,
            local_model: Some(LocalModelRuntime {
                provider: am_proto::LocalModelProviderKind::LmStudio,
                model: "local-qwen".into(),
                base_url: Some("http://127.0.0.1:1234/".into()),
                api_token: None,
            }),
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args.iter().any(|arg| arg == "--oss"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "oss_provider=\"lmstudio\""]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--model", "local-qwen"]));
    }

    #[test]
    fn builds_custom_local_provider_args_and_env_key() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: None,
            reasoning: None,
            local_model: Some(LocalModelRuntime {
                provider: am_proto::LocalModelProviderKind::LmStudio,
                model: "custom-model".into(),
                base_url: Some("http://localhost:4321".into()),
                api_token: Some("secret-token".into()),
            }),
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(!args.iter().any(|arg| arg == "--oss"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "model_provider=\"perpetual_local\""]));
        assert!(args.windows(2).any(|pair| {
            pair == [
                "-c",
                "model_providers.perpetual_local.base_url=\"http://localhost:4321/v1\"",
            ]
        }));
        assert!(args.windows(2).any(|pair| {
            pair == [
                "-c",
                "model_providers.perpetual_local.env_key=\"PERPETUAL_LOCAL_MODEL_TOKEN\"",
            ]
        }));
        assert_eq!(
            local_model_env(&spec),
            vec![(
                "PERPETUAL_LOCAL_MODEL_TOKEN".to_string(),
                "secret-token".to_string()
            )]
        );
    }

    #[test]
    fn builds_resume_args() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Continue".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::ReadOnly,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        assert_eq!(
            build_args(
                &spec,
                Some(&SessionRef {
                    agent_session_id: "thread-123".into(),
                })
            ),
            vec![
                "exec",
                "--json",
                "--color",
                "never",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "resume",
                "thread-123",
                "Continue",
            ]
        );
    }

    #[test]
    fn parses_thread_started() {
        let parsed = parse_line(&json!({
            "type": "thread.started",
            "thread_id": "67e55044-10b1-426f-9247-bb680e5fe0c8"
        }));
        assert!(matches!(
            &parsed.events[0],
            NormalizedEvent::SessionStarted { session_id }
                if session_id == "67e55044-10b1-426f-9247-bb680e5fe0c8"
        ));
        assert_eq!(parsed.terminal, None);
    }

    #[test]
    fn parses_agent_message() {
        let parsed = parse_line(&json!({
            "type": "item.completed",
            "item": {"id":"item_0","type":"agent_message","text":"Done."}
        }));
        assert!(matches!(
            &parsed.events[0],
            NormalizedEvent::AssistantText { text } if text == "Done."
        ));
    }

    #[test]
    fn parses_command_lifecycle() {
        let started = parse_line(&json!({
            "type": "item.started",
            "item": {
                "id":"item_0",
                "type":"command_execution",
                "command":"cargo test",
                "aggregated_output":"",
                "exit_code":null,
                "status":"in_progress"
            }
        }));
        assert!(matches!(
            &started.events[0],
            NormalizedEvent::ToolUse { name, input }
                if name == "Command" && input["command"] == "cargo test"
        ));

        let completed = parse_line(&json!({
            "type": "item.completed",
            "item": {
                "id":"item_0",
                "type":"command_execution",
                "command":"cargo test",
                "aggregated_output":"ok\n",
                "exit_code":0,
                "status":"completed"
            }
        }));
        assert!(matches!(
            &completed.events[0],
            NormalizedEvent::ToolResult { ok: true, summary } if summary == "ok\n"
        ));
    }

    #[test]
    fn parses_file_changes() {
        let parsed = parse_line(&json!({
            "type": "item.completed",
            "item": {
                "id": "item_0",
                "type": "file_change",
                "changes": [
                    {"path": "src/lib.rs", "kind": "update"},
                    {"path": "src/new.rs", "kind": "add"},
                    {"path": "src/old.rs", "kind": "delete"}
                ],
                "status": "completed"
            }
        }));

        assert!(matches!(
            &parsed.events[0],
            NormalizedEvent::FileChanged { path, change: ChangeKind::Modified }
                if path == std::path::Path::new("src/lib.rs")
        ));
        assert!(matches!(
            &parsed.events[1],
            NormalizedEvent::FileChanged { path, change: ChangeKind::Created }
                if path == std::path::Path::new("src/new.rs")
        ));
        assert!(matches!(
            &parsed.events[2],
            NormalizedEvent::FileChanged { path, change: ChangeKind::Deleted }
                if path == std::path::Path::new("src/old.rs")
        ));
    }

    #[test]
    fn parses_turn_completed_usage_without_session_end_event() {
        let parsed = parse_line(&json!({
            "type": "turn.completed",
            "usage": {
                "input_tokens": 100,
                "cached_input_tokens": 25,
                "output_tokens": 40,
                "reasoning_output_tokens": 10
            }
        }));

        assert!(matches!(
            &parsed.events[0],
            NormalizedEvent::TokenUsage {
                input: 100,
                output: 40
            }
        ));
        assert_eq!(parsed.terminal, Some(SessionStatus::Completed));
        assert!(!parsed
            .events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::SessionEnded { .. })));
    }

    #[test]
    fn parses_turn_failed_usage_limit() {
        let parsed = parse_line(&json!({
            "type": "turn.failed",
            "error": {"message": "Rate limit reached. Try again later."}
        }));

        assert!(matches!(
            &parsed.events[0],
            NormalizedEvent::UsageLimitReached { reset_at: None }
        ));
        assert_eq!(parsed.terminal, Some(SessionStatus::Failed));
    }

    #[test]
    fn ignores_unknown_items() {
        let parsed = parse_line(&json!({
            "type": "item.completed",
            "item": {"id": "item_0", "type": "future_tool", "value": 1}
        }));
        assert!(parsed.events.is_empty());
    }

    #[test]
    fn login_status_read_from_stderr() {
        // Regression: the real CLI prints the status to stderr.
        assert!(login_status_authenticated("", "Logged in using ChatGPT"));
        assert!(login_status_authenticated("Logged in using API key\n", ""));
        // "Not logged in" must not be mistaken for authenticated.
        assert!(!login_status_authenticated("", "Not logged in"));
        assert!(!login_status_authenticated("", ""));
    }
}
