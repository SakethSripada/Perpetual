//! Claude Code adapter.
//!
//! Drives `claude -p "<prompt>" --output-format stream-json --verbose` in the
//! task's worktree, using the user's logged-in **subscription** (we deliberately
//! do *not* use `--bare`, which would require an API key). Each JSONL line is
//! parsed into a [`NormalizedEvent`]. The exact stream shapes are verified
//! against the official headless docs; the parser is tolerant of version drift
//! and unit-tested against fixtures.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
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
    AgentAdapter, AgentError, AgentInstallStatus, AgentKind, NormalizedEvent, PermissionPolicy,
    QuotaWindowKind, SessionControl, SessionHandle, SessionRef, SessionSpec, SessionStatus,
};

const BIN: &str = "claude";
const CHANNEL_CAPACITY: usize = 256;
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }

    async fn launch(
        &self,
        spec: SessionSpec,
        resume: Option<SessionRef>,
    ) -> Result<SessionHandle, AgentError> {
        let args = build_args(&spec, resume.as_ref());
        let envs = policy_env(&spec);
        tracing::debug!(?args, worktree = ?spec.worktree, "launching claude");

        let budgeted = budgeted_host_run(&spec);
        let mut child = if budgeted {
            crate::runtime::spawn_host_piped_stdin(BIN, &args, &spec.worktree, &envs).await?
        } else {
            spawn_for_runtime_with_env(BIN, "claude", &args, &spec.worktree, &spec.runtime, &envs)
                .await?
        };
        let stdin = budgeted.then(|| child.take_stdin()).flatten();
        let stdout = child
            .take_stdout()
            .ok_or_else(|| AgentError::Spawn("no stdout pipe".into()))?;
        let stderr = child.take_stderr();

        let (tx, rx) = mpsc::channel::<NormalizedEvent>(CHANNEL_CAPACITY);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();

        let limits = spec.runtime.limits();
        tokio::spawn(drive(
            child,
            stdout,
            stderr,
            stdin,
            budgeted.then(|| spec.prompt.clone()),
            steer_rx,
            tx,
            cancel_rx,
            limits,
        ));

        Ok(SessionHandle {
            events: rx,
            control: SessionControl::with_steer(cancel_tx, steer_tx),
        })
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    async fn detect(&self) -> AgentInstallStatus {
        tokio::task::spawn_blocking(|| {
            let binary = find_binary(BIN);
            let version = binary.as_ref().and_then(|b| binary_version(b));
            AgentInstallStatus {
                kind: AgentKind::ClaudeCode,
                installed: binary.is_some(),
                // We can't cheaply verify subscription auth without spending
                // tokens; treat "installed" as the signal and let a run surface
                // an auth error if not logged in.
                authenticated: binary.is_some(),
                version,
                binary_path: binary,
            }
        })
        .await
        .unwrap_or(AgentInstallStatus {
            kind: AgentKind::ClaudeCode,
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

/// Build the `claude` argument vector. Every value is a discrete argument; the
/// prompt is never interpolated into a shell string.
fn build_args(spec: &SessionSpec, resume: Option<&SessionRef>) -> Vec<String> {
    let mut args = if budgeted_host_run(spec) {
        vec![
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
            "--verbose".to_string(),
        ]
    } else {
        vec![
            "-p".to_string(),
            spec.prompt.clone(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
            "--verbose".to_string(),
        ]
    };

    match spec.permission {
        PermissionPolicy::ReadOnly => {
            args.push("--permission-mode".into());
            args.push("plan".into());
        }
        PermissionPolicy::WorkspaceWrite | PermissionPolicy::Ask => {
            args.push("--permission-mode".into());
            // Headless Claude runs have no interactive approval channel. Keep
            // the non-interactive behavior explicit and safe for normal edits;
            // Codex exposes in-app approvals through its app-server transport.
            args.push("acceptEdits".into());
        }
        PermissionPolicy::Autonomous => {
            args.push("--dangerously-skip-permissions".into());
        }
    }
    if let Some(model) = normalize_model(spec.model.as_deref()) {
        args.push("--model".into());
        args.push(model);
    }
    if let Some(effort) = normalize_reasoning(spec.reasoning.as_deref()) {
        args.push("--effort".into());
        args.push(effort);
    }
    if let Some(policy) = spec.policy.as_ref() {
        push_policy_args(&mut args, policy);
    }
    if let Some(prior) = resume {
        args.push("--resume".into());
        args.push(prior.agent_session_id.clone());
    }
    args
}

fn budgeted_host_run(spec: &SessionSpec) -> bool {
    matches!(spec.runtime, crate::SessionRuntime::Host { .. })
        && spec
            .policy
            .as_ref()
            .and_then(|policy| policy.task_budget.as_ref())
            .is_some_and(|budget| !budget.is_unlimited())
}

async fn write_stream_input(stdin: &mut ChildStdin, text: &str) -> std::io::Result<()> {
    let line = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
    .to_string();
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

fn push_policy_args(args: &mut Vec<String>, policy: &crate::AgentPolicyRuntime) {
    if !policy.allowed_tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(policy.allowed_tools.join(" "));
    }
    let mut denied = policy.denied_tools.clone();
    denied.extend(
        policy
            .denied_context_globs
            .iter()
            .map(|glob| format!("Read({glob})")),
    );
    if !policy.denied_mcp_servers.is_empty() {
        denied.extend(policy.denied_mcp_servers.iter().map(|server| {
            if server == "*" {
                "mcp__*".to_string()
            } else {
                format!("mcp__{server}__*")
            }
        }));
    }
    if !denied.is_empty() {
        args.push("--disallowedTools".into());
        args.push(denied.join(" "));
    }
    if let Some(max) = policy.max_budget_usd {
        args.push("--max-budget-usd".into());
        args.push(format!("{max:.4}"));
    }
}

fn policy_env(spec: &SessionSpec) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    if let Some(policy) = spec.policy.as_ref() {
        if policy.disable_remote_mcp_connectors {
            envs.push(("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()));
        }
        if !policy.env_allowlist.is_empty() {
            envs.push((
                "PERPETUAL_POLICY_ENV_ALLOWLIST".into(),
                policy.env_allowlist.join(","),
            ));
        }
    }
    envs
}

fn normalize_model(model: Option<&str>) -> Option<String> {
    let value = clean_override(model)?;
    let lower = value.to_ascii_lowercase();
    if lower.contains("gpt-") || is_openai_reasoning_model(&lower) {
        return None;
    }
    Some(value.to_string())
}

fn normalize_reasoning(reasoning: Option<&str>) -> Option<String> {
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

fn is_openai_reasoning_model(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// The driver task: owns the child, streams parsed events, and is the sole
/// emitter of the terminal [`NormalizedEvent::SessionEnded`].
#[allow(clippy::too_many_arguments)]
async fn drive(
    mut child: ManagedChild,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    mut stdin: Option<ChildStdin>,
    initial_prompt: Option<String>,
    mut steer_rx: mpsc::UnboundedReceiver<String>,
    tx: mpsc::Sender<NormalizedEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
    limits: RuntimeLimits,
) {
    if let (Some(stdin), Some(prompt)) = (stdin.as_mut(), initial_prompt.as_deref()) {
        if write_stream_input(stdin, prompt).await.is_err() {
            let _ = tx
                .send(NormalizedEvent::Error {
                    message: "Claude stream input closed before the session started".into(),
                    retryable: false,
                })
                .await;
        }
    }
    // Capture stderr in the background for diagnostics / limit detection.
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
    let mut saw_result = false;
    let mut saw_structured_output = false;
    let mut seen_usage_message_ids = HashSet::new();
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
                            if value.get("type").and_then(|t| t.as_str()) == Some("result") {
                                saw_result = true;
                            }
                            let message_id = usage_message_id(&value);
                            for event in parse_line(&value) {
                                if matches!(&event, NormalizedEvent::TokenUsage { .. })
                                    && message_id.as_ref().is_some_and(|id| {
                                        !seen_usage_message_ids.insert(id.clone())
                                    })
                                {
                                    continue;
                                }
                                if tx.send(event).await.is_err() {
                                    cancelled = true; // receiver gone
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
                Ok(None) => break,         // EOF: process is finishing
                Err(e) => { tracing::warn!(error = %e, "stdout read error"); break; }
            },
            Some(instruction) = steer_rx.recv(), if stdin.is_some() => {
                if let Some(input) = stdin.as_mut() {
                    if write_stream_input(input, &instruction).await.is_err() {
                        break;
                    }
                }
            }
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

    // Terminate the whole process group if we cut the run short.
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

    // If the process failed before producing a structured result, surface stderr.
    if !cancelled && !success && !saw_result {
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

/// Parse a single stream-json line into zero or more normalized events. Does
/// **not** emit `SessionEnded` — the driver owns the terminal event.
pub(crate) fn parse_line(v: &Value) -> Vec<NormalizedEvent> {
    let mut out = Vec::new();
    out.extend(parse_quota_windows(v));
    match v.get("type").and_then(|t| t.as_str()) {
        Some("system") => match v.get("subtype").and_then(|s| s.as_str()) {
            Some("init") => {
                if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
                    out.push(NormalizedEvent::SessionStarted {
                        session_id: sid.to_string(),
                    });
                }
            }
            // Claude may emit transient rate-limit retries and then recover.
            // Treat terminal result/stderr limit messages as handoff signals;
            // a retry notice alone is not enough to switch agents.
            Some("api_retry") if v.get("error").and_then(|e| e.as_str()) == Some("rate_limit") => {}
            _ => {}
        },
        Some("assistant") => {
            if let Some(usage) = v
                .pointer("/message/usage")
                .or_else(|| v.get("usage"))
                .and_then(token_usage_event)
            {
                out.push(usage);
            }
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                if !text.trim().is_empty() {
                                    out.push(NormalizedEvent::AssistantText {
                                        text: text.to_string(),
                                    });
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            out.push(NormalizedEvent::ToolUse { name, input });
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("stream_event") => {
            let event = v.get("event").unwrap_or(v);
            if event.get("type").and_then(|t| t.as_str()) == Some("content_block_delta")
                && event.pointer("/delta/type").and_then(|t| t.as_str()) == Some("text_delta")
            {
                if let Some(delta) = event.pointer("/delta/text").and_then(|t| t.as_str()) {
                    if !delta.is_empty() {
                        out.push(NormalizedEvent::AssistantTextDelta {
                            delta: delta.to_string(),
                        });
                    }
                }
            }
        }
        Some("user") => {
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        let is_error = block
                            .get("is_error")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false);
                        let summary = stringify_tool_content(block.get("content"));
                        out.push(NormalizedEvent::ToolResult {
                            ok: !is_error,
                            summary,
                        });
                    }
                }
            }
        }
        Some("result") => {
            if let Some(usage) = v.get("usage").and_then(token_usage_event) {
                out.push(usage);
            }
            // Actions the agent attempted but the chosen permission level blocked.
            // Surfacing these lets the user re-run with more autonomy ("approval").
            if let Some(detail) = permission_denials_detail(v.get("permission_denials")) {
                out.push(NormalizedEvent::AwaitingApproval { detail });
            }
            let is_error = v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
            if is_error {
                let text = v.get("result").and_then(|r| r.as_str()).unwrap_or("");
                match detect_usage_limit(text) {
                    Some(reset_at) => out.push(NormalizedEvent::UsageLimitReached { reset_at }),
                    None if let Some(message) = detect_network_error(text) => {
                        out.push(NormalizedEvent::NetworkUnavailable { message });
                    }
                    None if !text.trim().is_empty() => out.push(NormalizedEvent::Error {
                        message: truncate(text, 2000),
                        retryable: false,
                    }),
                    None => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Claude subscription streams may report quota updates as Agent SDK-style
/// `rate_limit_event` messages or as a statusline-shaped `rate_limits` object.
/// Accept both shapes so the core can enforce the selected rolling windows
/// without exposing the provider payload.
fn parse_quota_windows(value: &Value) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    if value.get("rate_limit_type").is_some() || value.get("rateLimitType").is_some() {
        if let Some(event) = parse_quota_sample(value, None) {
            events.push(event);
        }
    }
    if let Some(info) = value.get("rate_limit_info") {
        if let Some(event) = parse_quota_sample(info, None) {
            events.push(event);
        }
    }
    if let Some(info) = value.get("rateLimitInfo") {
        if let Some(event) = parse_quota_sample(info, None) {
            events.push(event);
        }
    }
    if let Some(rate_limits) = value.get("rate_limits").or_else(|| value.get("rateLimits")) {
        if let Some(map) = rate_limits.as_object() {
            for (window, info) in map {
                if let Some(event) = parse_quota_sample(info, Some(window)) {
                    events.push(event);
                }
            }
        }
    }
    for key in ["data", "event", "payload"] {
        if let Some(nested) = value.get(key) {
            events.extend(parse_quota_windows(nested));
        }
    }
    events
}

fn parse_quota_sample(value: &Value, window_hint: Option<&str>) -> Option<NormalizedEvent> {
    let map = value.as_object()?;
    let raw = map.get("raw").and_then(Value::as_object);
    let window_name = map
        .get("rate_limit_type")
        .or_else(|| map.get("rateLimitType"))
        .or_else(|| map.get("window"))
        .or_else(|| map.get("window_type"))
        .and_then(Value::as_str)
        .or_else(|| {
            raw.and_then(|raw| {
                raw.get("rate_limit_type")
                    .or_else(|| raw.get("rateLimitType"))
                    .or_else(|| raw.get("window"))
                    .and_then(Value::as_str)
            })
        })
        .or(window_hint)?
        .to_ascii_lowercase();
    let window = if window_name.contains("five") || window_name.contains("5h") {
        QuotaWindowKind::FiveHour
    } else if window_name.contains("seven")
        || window_name.contains("7d")
        || window_name.contains("week")
    {
        QuotaWindowKind::Weekly
    } else {
        return None;
    };
    let used = map
        .get("used_percentage")
        .or_else(|| map.get("used_percent"))
        .or_else(|| map.get("usedPercent"))
        .or_else(|| map.get("percent_used"))
        .or_else(|| map.get("percentUsed"))
        .or_else(|| map.get("utilization"))
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .or_else(|| {
            map.get("remaining_percentage")
                .or_else(|| map.get("remaining_percent"))
                .or_else(|| map.get("remainingPercent"))
                .and_then(|value| {
                    value
                        .as_f64()
                        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                })
                .map(|remaining| 100.0 - remaining)
        })?;
    let used = if map.get("utilization").is_some() && (0.0..=1.0).contains(&used) {
        used * 100.0
    } else {
        used
    };
    Some(NormalizedEvent::QuotaWindow {
        window,
        used_percent: used.clamp(0.0, 100.0),
        reset_at: map
            .get("resets_at")
            .or_else(|| map.get("reset_at"))
            .or_else(|| map.get("resetsAt"))
            .and_then(parse_reset_at),
    })
}

fn parse_reset_at(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|value| value.with_timezone(&Utc));
    }
    let seconds = value.as_f64()?;
    let seconds = if seconds > 10_000_000_000.0 {
        seconds / 1_000.0
    } else {
        seconds
    };
    DateTime::from_timestamp(seconds as i64, 0)
}

fn token_usage_event(usage: &Value) -> Option<NormalizedEvent> {
    let input = usage
        .get("input_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let cached_input = usage
        .get("cache_read_input_tokens")
        .or_else(|| usage.get("cached_input_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    (input + cached_input + output > 0).then_some(NormalizedEvent::TokenUsage {
        input: input.saturating_add(cached_input),
        output,
    })
}

fn usage_message_id(value: &Value) -> Option<String> {
    value
        .pointer("/message/id")
        .or_else(|| value.pointer("/message/message_id"))
        .or_else(|| value.get("message_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Summarize a `result.permission_denials` array into a human detail string, or
/// `None` when there were no denials. Each entry names the blocked `tool_name`.
fn permission_denials_detail(value: Option<&Value>) -> Option<String> {
    let arr = value?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for entry in arr {
        let name = entry
            .get("tool_name")
            .and_then(|n| n.as_str())
            .unwrap_or("a tool");
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    let count = arr.len();
    Some(format!(
        "{count} action{} blocked by the current permission level: {}",
        if count == 1 { "" } else { "s" },
        names.join(", ")
    ))
}

fn stringify_tool_content(content: Option<&Value>) -> String {
    let s = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    truncate(&s, 800)
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
    fn builds_args_with_model_and_effort() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: Some("opus".into()),
            reasoning: Some("max".into()),
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        assert_eq!(
            build_args(&spec, None),
            vec![
                "-p",
                "Implement it",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "opus",
                "--effort",
                "max",
            ]
        );
    }

    #[test]
    fn passes_full_claude_model_id_through() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Implement it".into(),
            model: Some("claude-opus-4-8".into()),
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--model", "claude-opus-4-8"]));
    }

    #[test]
    fn drops_codex_model_but_forwards_provider_validated_effort_for_claude() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Continue".into(),
            model: Some("gpt-5.5".into()),
            reasoning: Some("minimal".into()),
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(!args.iter().any(|arg| arg == "--model" || arg == "gpt-5.5"));
        assert!(args.windows(2).any(|pair| pair == ["--effort", "minimal"]));
    }

    #[test]
    fn edit_mode_uses_noninteractive_accept_edits() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Edit".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };
        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|p| p == ["--permission-mode", "acceptEdits"]));
        assert!(!args.iter().any(|a| a == "--settings"));
    }

    #[test]
    fn ask_mode_uses_noninteractive_accept_edits() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Do it".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::Ask,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };
        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|p| p == ["--permission-mode", "acceptEdits"]));
    }

    #[test]
    fn read_only_runs_use_claude_plan_mode() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Create a plan for fixing auth.".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::ReadOnly,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };

        let args = build_args(&spec, None);
        assert!(args.windows(2).any(|p| p == ["--permission-mode", "plan"]));
    }

    #[test]
    fn parses_init_session_id() {
        let v = json!({"type":"system","subtype":"init","session_id":"abc-123","model":"claude"});
        let events = parse_line(&v);
        assert!(
            matches!(&events[0], NormalizedEvent::SessionStarted { session_id } if session_id == "abc-123")
        );
    }

    #[test]
    fn parses_assistant_text_and_tool_use() {
        let v = json!({
            "type":"assistant",
            "message":{"role":"assistant","content":[
                {"type":"text","text":"Editing the file"},
                {"type":"tool_use","name":"Edit","input":{"file_path":"a.rs"}}
            ]}
        });
        let events = parse_line(&v);
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], NormalizedEvent::AssistantText { text } if text == "Editing the file")
        );
        assert!(matches!(&events[1], NormalizedEvent::ToolUse { name, .. } if name == "Edit"));
    }

    #[test]
    fn enables_and_parses_partial_assistant_text() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "hello".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::ReadOnly,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };
        assert!(build_args(&spec, None)
            .iter()
            .any(|arg| arg == "--include-partial-messages"));

        let events = parse_line(&json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Smooth" }
            }
        }));
        assert!(matches!(
            &events[0],
            NormalizedEvent::AssistantTextDelta { delta } if delta == "Smooth"
        ));
    }

    #[test]
    fn budgeted_host_runs_use_streaming_input() {
        let policy = crate::AgentPolicyRuntime {
            task_budget: Some(am_proto::TaskBudget::Tokens {
                limit_tokens: 50_000,
            }),
            ..Default::default()
        };
        let spec = SessionSpec {
            worktree: "/tmp/wt".into(),
            prompt: "go".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::WorkspaceWrite,
            runtime: crate::SessionRuntime::default(),
            policy: Some(policy),
            approver: None,
        };
        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--input-format", "stream-json"]));
        assert!(!args.iter().any(|arg| arg == "-p"));
    }

    #[test]
    fn parses_tool_result() {
        let v = json!({
            "type":"user",
            "message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}
            ]}
        });
        let events = parse_line(&v);
        assert!(matches!(
            &events[0],
            NormalizedEvent::ToolResult { ok: true, .. }
        ));
    }

    #[test]
    fn detects_rate_limit_retry() {
        let v = json!({"type":"system","subtype":"api_retry","error":"rate_limit","attempt":1});
        let events = parse_line(&v);
        assert!(events.is_empty());
    }

    #[test]
    fn result_usage_and_limit() {
        let v = json!({
            "type":"result","subtype":"error_during_execution","is_error":true,
            "result":"Claude usage limit reached. Try again later.",
            "usage":{"input_tokens":100,"output_tokens":50}
        });
        let events = parse_line(&v);
        assert!(events.iter().any(|e| matches!(
            e,
            NormalizedEvent::TokenUsage {
                input: 100,
                output: 50
            }
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::UsageLimitReached { .. })));
        // No SessionEnded from the parser — the driver owns it.
        assert!(!events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::SessionEnded { .. })));
    }

    #[test]
    fn assistant_usage_includes_cached_input_and_message_ids() {
        let v = json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 25,
                    "output_tokens": 10
                }
            }
        });
        let events = parse_line(&v);
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::TokenUsage {
                input: 125,
                output: 10
            }]
        ));
        assert_eq!(usage_message_id(&v).as_deref(), Some("msg_1"));
    }

    #[test]
    fn parses_claude_five_hour_and_weekly_rate_limits() {
        let events = parse_line(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "rate_limit_type": "five_hour",
                "utilization": 0.125,
                "resets_at": "2026-07-22T18:00:00Z"
            }
        }));
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::QuotaWindow {
                window: QuotaWindowKind::FiveHour,
                used_percent,
                reset_at: Some(_),
            }] if (*used_percent - 12.5).abs() < f64::EPSILON
        ));

        let events = parse_line(&json!({
            "rate_limits": {
                "seven_day": {
                    "used_percentage": 32.0,
                    "resets_at": 1785000000_i64
                }
            }
        }));
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::QuotaWindow {
                window: QuotaWindowKind::Weekly,
                used_percent,
                ..
            }] if (*used_percent - 32.0).abs() < f64::EPSILON
        ));

        let events = parse_line(&json!({
            "type": "rate_limit_event",
            "data": {
                "rateLimitInfo": {
                    "rateLimitType": "five_hour",
                    "utilization": "0.25",
                    "resetsAt": 1785000000_i64
                }
            }
        }));
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::QuotaWindow {
                window: QuotaWindowKind::FiveHour,
                used_percent,
                reset_at: Some(_),
            }] if (*used_percent - 25.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn ignores_rate_limit_payloads_without_a_supported_window_or_usage() {
        assert!(parse_line(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "rate_limit_type": "overage" }
        }))
        .is_empty());
    }

    #[test]
    fn result_monthly_spend_limit() {
        let v = json!({
            "type":"result","subtype":"error_during_execution","is_error":true,
            "result":"You've hit your monthly spend limit · raise it at claude.ai/settings/usage"
        });
        let events = parse_line(&v);
        assert!(events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::UsageLimitReached { reset_at: None })));
        assert!(!events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::Error { .. })));
    }

    #[test]
    fn result_with_permission_denials_emits_awaiting_approval() {
        let v = json!({
            "type":"result","subtype":"success","is_error":false,"result":"done",
            "permission_denials":[
                {"tool_name":"Bash","tool_input":{"command":"rm -rf /"}},
                {"tool_name":"Bash","tool_input":{"command":"curl evil"}},
                {"tool_name":"WebFetch","tool_input":{}}
            ]
        });
        let events = parse_line(&v);
        let detail = events.iter().find_map(|e| match e {
            NormalizedEvent::AwaitingApproval { detail } => Some(detail.clone()),
            _ => None,
        });
        let detail = detail.expect("awaiting_approval event");
        assert!(detail.contains("3 actions blocked"), "{detail}");
        assert!(
            detail.contains("Bash") && detail.contains("WebFetch"),
            "{detail}"
        );
    }

    #[test]
    fn result_without_denials_has_no_approval_event() {
        let v =
            json!({"type":"result","subtype":"success","is_error":false,"permission_denials":[]});
        assert!(!parse_line(&v)
            .iter()
            .any(|e| matches!(e, NormalizedEvent::AwaitingApproval { .. })));
    }

    #[test]
    fn ignores_unknown_and_partial() {
        assert!(parse_line(&json!({"type":"stream_event","event":{}})).is_empty());
        assert!(parse_line(&json!({"type":"system","subtype":"other"})).is_empty());
    }
}
