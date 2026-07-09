//! Cursor CLI adapter.
//!
//! Drives Cursor's headless agent (`cursor-agent` or `agent`) with stream-json
//! output and maps the stream into AgentManager's normalized event model.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::detect::{binary_version, find_binary};
use crate::limits::detect_usage_limit;
use crate::network::detect_network_error;
use crate::process::ManagedChild;
use crate::runtime::{
    buffered_diagnostics, push_diagnostic_line, spawn_for_runtime, with_diagnostics, RuntimeLimits,
};
use crate::{
    AgentAdapter, AgentError, AgentInstallStatus, AgentKind, NormalizedEvent, PermissionPolicy,
    SessionControl, SessionHandle, SessionRef, SessionSpec, SessionStatus,
};

const PRIMARY_BIN: &str = "cursor-agent";
const FALLBACK_BIN: &str = "agent";
const CHANNEL_CAPACITY: usize = 256;
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct CursorAdapter;

impl CursorAdapter {
    pub fn new() -> Self {
        Self
    }

    async fn launch(
        &self,
        spec: SessionSpec,
        resume: Option<SessionRef>,
    ) -> Result<SessionHandle, AgentError> {
        let host_bin = match &spec.runtime {
            crate::SessionRuntime::Host { .. } => {
                cursor_binary_name().ok_or(AgentError::NotInstalled(AgentKind::Cursor))?
            }
            crate::SessionRuntime::DockerSandbox { .. } => {
                cursor_binary_name().unwrap_or_else(|| PRIMARY_BIN.to_string())
            }
        };
        let args = build_args(&spec, resume.as_ref());
        tracing::debug!(?args, worktree = ?spec.worktree, "launching cursor");

        let mut child =
            spawn_for_runtime(&host_bin, "cursor", &args, &spec.worktree, &spec.runtime).await?;
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
impl AgentAdapter for CursorAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Cursor
    }

    async fn detect(&self) -> AgentInstallStatus {
        tokio::task::spawn_blocking(|| {
            let binary = cursor_binary();
            let version = binary.as_ref().and_then(|b| binary_version(b));
            let authenticated = binary
                .as_ref()
                .map(|b| cursor_authenticated(b))
                .unwrap_or(false);
            AgentInstallStatus {
                kind: AgentKind::Cursor,
                installed: binary.is_some(),
                authenticated,
                version,
                binary_path: binary,
            }
        })
        .await
        .unwrap_or(AgentInstallStatus {
            kind: AgentKind::Cursor,
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

fn cursor_binary_name() -> Option<String> {
    if find_binary(PRIMARY_BIN).is_some() {
        return Some(PRIMARY_BIN.to_string());
    }
    let agent = find_binary(FALLBACK_BIN)?;
    binary_looks_like_cursor(&agent).then(|| FALLBACK_BIN.to_string())
}

fn cursor_binary() -> Option<PathBuf> {
    find_binary(PRIMARY_BIN).or_else(|| {
        let agent = find_binary(FALLBACK_BIN)?;
        binary_looks_like_cursor(&agent).then_some(agent)
    })
}

fn binary_looks_like_cursor(binary: &Path) -> bool {
    binary_version(binary)
        .map(|v| v.to_ascii_lowercase().contains("cursor"))
        .unwrap_or_else(|| {
            cursor_status_output(binary)
                .to_ascii_lowercase()
                .contains("cursor")
        })
}

fn cursor_authenticated(binary: &Path) -> bool {
    let out = cursor_status_output(binary).to_ascii_lowercase();
    if out.is_empty() {
        return false;
    }
    (out.contains("authenticated") || out.contains("logged in") || out.contains("signed in"))
        && !(out.contains("not authenticated")
            || out.contains("not logged in")
            || out.contains("signed out"))
}

fn cursor_status_output(binary: &Path) -> String {
    let output = Command::new(binary)
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) => format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(_) => String::new(),
    }
}

fn build_args(spec: &SessionSpec, resume: Option<&SessionRef>) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        spec.prompt.clone(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--stream-partial-output".to_string(),
    ];

    match spec.permission {
        PermissionPolicy::ReadOnly => {
            args.push("--mode".into());
            args.push("plan".into());
        }
        // Cursor has no live-approval protocol; treat Ask like WorkspaceWrite.
        PermissionPolicy::WorkspaceWrite | PermissionPolicy::Ask => args.push("--force".into()),
        PermissionPolicy::Autonomous => args.push("--yolo".into()),
    }

    if let Some(model) = clean_override(spec.model.as_deref()) {
        args.push("--model".into());
        args.push(model.to_string());
    }

    if let Some(prior) = resume {
        args.push("--resume".into());
        args.push(prior.agent_session_id.clone());
    }
    args
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
    let mut terminal_status: Option<SessionStatus> = None;
    let mut saw_structured_output = false;
    let mut state = CursorStreamState::default();
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
                            let parsed = parse_line(&value, &mut state);
                            if parsed.terminal.is_some() {
                                terminal_status = parsed.terminal;
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
    } else if let Some(status) = terminal_status {
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

    if !cancelled && !success && terminal_status.is_none() {
        let err = buffered_diagnostics(&stdout_buf, &stderr_buf);
        let err = err.trim();
        if !err.is_empty() {
            push_error_or_limit(&tx, err).await;
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
pub(crate) struct CursorStreamState {
    assistant_buffer: String,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedCursorLine {
    pub events: Vec<NormalizedEvent>,
    pub terminal: Option<SessionStatus>,
}

pub(crate) fn parse_line(v: &Value, state: &mut CursorStreamState) -> ParsedCursorLine {
    let mut parsed = ParsedCursorLine::default();
    let kind = v
        .get("type")
        .or_else(|| v.get("event"))
        .or_else(|| v.get("kind"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    if kind.contains("session") || kind == "system" || kind == "init" {
        if let Some(session_id) = string_at_any(v, &["/session_id", "/session/id", "/id"]) {
            parsed.events.push(NormalizedEvent::SessionStarted {
                session_id: session_id.to_string(),
            });
        }
    }

    if kind.contains("complete") || kind == "done" {
        parsed.terminal = Some(SessionStatus::Completed);
    } else if kind.contains("failed") || kind == "error" {
        parsed.terminal = Some(SessionStatus::Failed);
    }

    if let Some(usage) = parse_usage(v.get("usage").or_else(|| v.get("token_usage"))) {
        parsed.events.push(usage);
    }

    if kind.contains("tool") && !kind.contains("result") {
        let name = string_at_any(v, &["/name", "/tool/name", "/tool", "/function/name"])
            .unwrap_or("tool")
            .to_string();
        let input = v
            .get("input")
            .or_else(|| v.get("arguments"))
            .or_else(|| v.pointer("/tool/input"))
            .cloned()
            .unwrap_or(Value::Null);
        parsed.events.push(NormalizedEvent::ToolUse { name, input });
    } else if kind.contains("tool") && kind.contains("result") {
        let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or_else(|| {
            !matches!(
                string_at_any(v, &["/status", "/state"]),
                Some("failed" | "error")
            )
        });
        let summary = string_at_any(v, &["/summary", "/result", "/output", "/content"])
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string());
        parsed.events.push(NormalizedEvent::ToolResult {
            ok,
            summary: truncate(&summary, 800),
        });
    }

    if let Some(message) = string_at_any(v, &["/error/message", "/message/error", "/error"]) {
        push_error_or_limit_sync(&mut parsed.events, message);
    }

    if let Some(text) = assistant_text(v) {
        let is_partial = kind.contains("partial")
            || kind.contains("delta")
            || v.get("partial").and_then(|b| b.as_bool()).unwrap_or(false);
        push_assistant_text(&mut parsed.events, state, text, is_partial);
    }

    parsed
}

fn assistant_text(v: &Value) -> Option<&str> {
    string_at_any(
        v,
        &[
            "/text",
            "/delta",
            "/content",
            "/message/content",
            "/message/text",
            "/assistant/text",
        ],
    )
}

fn push_assistant_text(
    events: &mut Vec<NormalizedEvent>,
    state: &mut CursorStreamState,
    text: &str,
    is_partial: bool,
) {
    if text.trim().is_empty() {
        return;
    }
    let delta = if text.starts_with(&state.assistant_buffer) {
        text[state.assistant_buffer.len()..].to_string()
    } else {
        text.to_string()
    };
    if !delta.trim().is_empty() {
        events.push(NormalizedEvent::AssistantText { text: delta });
    }
    if is_partial {
        state.assistant_buffer = text.to_string();
    } else {
        state.assistant_buffer.clear();
    }
}

fn string_at_any<'a>(v: &'a Value, paths: &[&str]) -> Option<&'a str> {
    for path in paths {
        if let Some(s) = v.pointer(path).and_then(|value| value.as_str()) {
            return Some(s);
        }
    }
    None
}

fn parse_usage(usage: Option<&Value>) -> Option<NormalizedEvent> {
    let usage = usage?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("input"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("output"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    if input + output > 0 {
        Some(NormalizedEvent::TokenUsage { input, output })
    } else {
        None
    }
}

async fn push_error_or_limit(tx: &mpsc::Sender<NormalizedEvent>, message: &str) {
    match detect_usage_limit(message) {
        Some(reset_at) => {
            let _ = tx
                .send(NormalizedEvent::UsageLimitReached { reset_at })
                .await;
        }
        None if let Some(message) = detect_network_error(message) => {
            let _ = tx
                .send(NormalizedEvent::NetworkUnavailable { message })
                .await;
        }
        None => {
            let _ = tx
                .send(NormalizedEvent::Error {
                    message: truncate(message, 2000),
                    retryable: false,
                })
                .await;
        }
    }
}

fn push_error_or_limit_sync(out: &mut Vec<NormalizedEvent>, message: &str) {
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
    use crate::runtime::{RuntimeLimits, SessionRuntime};
    use serde_json::json;

    fn spec(permission: PermissionPolicy) -> SessionSpec {
        SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Do it".into(),
            model: Some("auto".into()),
            reasoning: None,
            local_model: None,
            mcp: None,
            permission,
            runtime: SessionRuntime::Host {
                limits: RuntimeLimits::default(),
            },
            policy: None,
            approver: None,
        }
    }

    #[test]
    fn builds_args_for_permission_modes() {
        assert!(build_args(&spec(PermissionPolicy::ReadOnly), None)
            .windows(2)
            .any(|pair| pair == ["--mode", "plan"]));
        assert!(build_args(&spec(PermissionPolicy::WorkspaceWrite), None)
            .iter()
            .any(|arg| arg == "--force"));
        assert!(build_args(&spec(PermissionPolicy::Autonomous), None)
            .iter()
            .any(|arg| arg == "--yolo"));
    }

    #[test]
    fn builds_resume_args() {
        let args = build_args(
            &spec(PermissionPolicy::WorkspaceWrite),
            Some(&SessionRef {
                agent_session_id: "cur-123".into(),
            }),
        );
        assert!(args.windows(2).any(|pair| pair == ["--resume", "cur-123"]));
    }

    #[test]
    fn parses_partial_text_without_duplicate_final() {
        let mut state = CursorStreamState::default();
        let first = parse_line(
            &json!({"type":"assistant.partial","text":"Hello"}),
            &mut state,
        );
        let second = parse_line(
            &json!({"type":"assistant.partial","text":"Hello world"}),
            &mut state,
        );
        let final_line = parse_line(
            &json!({"type":"assistant","text":"Hello world"}),
            &mut state,
        );

        assert_eq!(first.events.len(), 1);
        assert!(
            matches!(&first.events[0], NormalizedEvent::AssistantText { text } if text == "Hello")
        );
        assert!(
            matches!(&second.events[0], NormalizedEvent::AssistantText { text } if text == " world")
        );
        assert!(final_line.events.is_empty());
    }

    #[test]
    fn parses_tool_and_limit_error() {
        let mut state = CursorStreamState::default();
        let tool = parse_line(
            &json!({"type":"tool_call","name":"Edit","input":{"path":"a.rs"}}),
            &mut state,
        );
        assert!(matches!(&tool.events[0], NormalizedEvent::ToolUse { name, .. } if name == "Edit"));

        let err = parse_line(
            &json!({"type":"error","message":{"error":"rate limit reached"}}),
            &mut state,
        );
        assert!(err
            .events
            .iter()
            .any(|event| matches!(event, NormalizedEvent::UsageLimitReached { .. })));
    }
}
