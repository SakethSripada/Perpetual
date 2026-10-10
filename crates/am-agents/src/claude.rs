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
use serde_json::{json, Value};
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
    AgentAdapter, AgentError, AgentInstallStatus, AgentKind, ApprovalAsk, ApprovalDecision,
    ApprovalKind, ApprovalResponder, NormalizedEvent, PermissionPolicy, SessionControl,
    SessionHandle, SessionRef, SessionSpec, SessionStatus,
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
        mut spec: SessionSpec,
        resume: Option<SessionRef>,
    ) -> Result<SessionHandle, AgentError> {
        crate::attachments::unpack(&spec.prompt)?;
        if !matches!(spec.runtime, crate::SessionRuntime::Host { .. }) {
            spec.prompt = crate::attachments::unpack(&spec.prompt)?.0;
        }
        let args = build_args(&spec, resume.as_ref());
        let envs = policy_env(&spec);
        tracing::debug!(?args, worktree = ?spec.worktree, "launching claude");

        let streaming = stream_input(&spec);
        let mut child = if streaming {
            crate::runtime::spawn_host_piped_stdin(BIN, &args, &spec.worktree, &envs).await?
        } else {
            spawn_for_runtime_with_env(BIN, "claude", &args, &spec.worktree, &spec.runtime, &envs)
                .await?
        };
        let stdin = streaming.then(|| child.take_stdin()).flatten();
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
            streaming.then(|| spec.prompt.clone()),
            spec.approver,
            spec.worktree,
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
    let mut args = if stream_input(spec) {
        vec![
            "-p".to_string(),
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

    if spec.approver.is_some() && matches!(spec.runtime, crate::SessionRuntime::Host { .. }) {
        args.push("--permission-prompt-tool".into());
        args.push("stdio".into());
    }

    match spec.permission {
        PermissionPolicy::ReadOnly => {
            args.push("--permission-mode".into());
            args.push("plan".into());
        }
        PermissionPolicy::WorkspaceWrite => {
            args.push("--permission-mode".into());
            args.push("acceptEdits".into());
        }
        PermissionPolicy::Ask => {
            args.push("--permission-mode".into());
            args.push(
                if spec.approver.is_some() {
                    "manual"
                } else {
                    "acceptEdits"
                }
                .into(),
            );
        }
        PermissionPolicy::Autonomous => {
            args.push("--permission-mode".into());
            args.push("bypassPermissions".into());
            args.push("--dangerously-skip-permissions".into());
            // Skipping approval prompts does not disable Claude's Bash sandbox.
            // Full access must override inherited user/project sandbox settings
            // for this invocation, including when resuming a saved session.
            args.push("--settings".into());
            args.push(json!({"sandbox": {"enabled": false}}).to_string());
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

fn stream_input(spec: &SessionSpec) -> bool {
    (spec.prompt.contains("<perpetual-attachments>")
        && matches!(spec.runtime, crate::SessionRuntime::Host { .. }))
        || budgeted_host_run(spec)
        || (spec.approver.is_some() && matches!(spec.runtime, crate::SessionRuntime::Host { .. }))
}

fn stream_user_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": crate::attachments::claude_content(text)
        }
    })
    .to_string()
}

fn handle_control_request(
    value: Value,
    approver: Option<ApprovalResponder>,
    input_tx: mpsc::Sender<String>,
    worktree: std::path::PathBuf,
) {
    let Some(request_id) = value
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let request = value.get("request").cloned().unwrap_or(Value::Null);
    tokio::spawn(async move {
        let response = if request.get("subtype").and_then(Value::as_str) == Some("can_use_tool") {
            let tool_name = request
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
            let command = input.get("command").and_then(Value::as_str);
            let decision = if let Some(approver) = approver {
                approver
                    .ask(ApprovalAsk {
                        kind: if command.is_some() {
                            ApprovalKind::Command
                        } else {
                            ApprovalKind::Tool
                        },
                        tool_name: tool_name.to_string(),
                        command: command.map(|text| vec![text.to_string()]),
                        cwd: Some(worktree.to_string_lossy().to_string()),
                        input: input.clone(),
                        reason: request
                            .get("decision_reason")
                            .or_else(|| request.get("description"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                    .await
            } else {
                ApprovalDecision::Deny
            };
            if decision.is_allow() {
                let mut allow = json!({"behavior": "allow", "updatedInput": input});
                if decision == ApprovalDecision::AllowForSession {
                    let suggestions = request
                        .get("permission_suggestions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|suggestion| {
                            suggestion.get("behavior").and_then(Value::as_str) == Some("allow")
                        })
                        .map(|mut suggestion| {
                            suggestion["destination"] = json!("session");
                            suggestion
                        })
                        .collect::<Vec<_>>();
                    if !suggestions.is_empty() {
                        allow["updatedPermissions"] = json!(suggestions);
                    }
                }
                allow
            } else {
                json!({
                    "behavior": "deny",
                    "message": "Denied in Perpetual",
                    "interrupt": decision == ApprovalDecision::Abort,
                })
            }
        } else {
            json!({"behavior": "deny", "message": "Unsupported Claude control request"})
        };
        let reply = json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": response,
            }
        });
        let _ = input_tx.send(reply.to_string()).await;
    });
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
    let mut envs = spec
        .policy
        .as_ref()
        .map(|policy| policy.launch_env.clone())
        .unwrap_or_default();
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
    stdin: Option<ChildStdin>,
    initial_prompt: Option<String>,
    approver: Option<ApprovalResponder>,
    worktree: std::path::PathBuf,
    mut steer_rx: mpsc::UnboundedReceiver<String>,
    tx: mpsc::Sender<NormalizedEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
    limits: RuntimeLimits,
) {
    let (mut input_tx, input_task) = if let Some(mut stdin) = stdin {
        let (sender, mut receiver) = mpsc::channel::<String>(32);
        let task = tokio::spawn(async move {
            while let Some(line) = receiver.recv().await {
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
            }
            Ok::<(), std::io::Error>(())
        });
        (Some(sender), Some(task))
    } else {
        (None, None)
    };
    if let (Some(sender), Some(prompt)) = (input_tx.as_ref(), initial_prompt.as_deref()) {
        if sender.send(stream_user_line(prompt)).await.is_err() {
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
    let mut usage_totals = MessageUsageTotals::default();
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
                            if value.get("type").and_then(Value::as_str) == Some("control_request") {
                                if let Some(sender) = input_tx.as_ref() {
                                    handle_control_request(value.clone(), approver.clone(), sender.clone(), worktree.clone());
                                }
                                continue;
                            }
                            if value.get("type").and_then(|t| t.as_str()) == Some("result") {
                                saw_result = true;
                                // Print-mode stream input is bidirectional. Once Claude has
                                // emitted its terminal result, close our side of the pipe so
                                // the process can exit instead of waiting for another turn.
                                drop(input_tx.take());
                            }
                            let message_id = usage_message_id(&value);
                            let is_result = value.get("type").and_then(Value::as_str) == Some("result");
                            for event in parse_line(&value) {
                                let event = match event {
                                    NormalizedEvent::TokenUsage { input, output } if is_result => {
                                        // Claude's result usage summarizes the whole run. Emit
                                        // only tokens missing from message-level reports.
                                        let (input, output) = usage_totals.result_delta(input, output);
                                        if input == 0 && output == 0 { continue; }
                                        NormalizedEvent::TokenUsage { input, output }
                                    }
                                    NormalizedEvent::TokenUsage { input, output } => {
                                        if message_id.as_ref().is_some_and(|id| {
                                            !seen_usage_message_ids.insert(id.clone())
                                        }) { continue; }
                                        usage_totals.record_message(input, output);
                                        NormalizedEvent::TokenUsage { input, output }
                                    }
                                    other => other,
                                };
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
            Some(instruction) = steer_rx.recv(), if input_tx.is_some() => {
                if let Some(sender) = input_tx.as_ref() {
                    if sender.send(stream_user_line(&instruction)).await.is_err() {
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

    drop(input_tx);
    if let Some(task) = input_task {
        task.abort();
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

#[derive(Default)]
struct MessageUsageTotals {
    input: u64,
    output: u64,
}

impl MessageUsageTotals {
    fn record_message(&mut self, input: u64, output: u64) {
        self.input = self.input.saturating_add(input);
        self.output = self.output.saturating_add(output);
    }

    fn result_delta(&self, input: u64, output: u64) -> (u64, u64) {
        (
            input.saturating_sub(self.input),
            output.saturating_sub(self.output),
        )
    }
}

/// Parse a single stream-json line into zero or more normalized events. Does
/// **not** emit `SessionEnded` — the driver owns the terminal event.
pub(crate) fn parse_line(v: &Value) -> Vec<NormalizedEvent> {
    let mut out = Vec::new();
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
                            let call_id = block
                                .get("id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string);
                            out.push(NormalizedEvent::ToolUse {
                                call_id,
                                name,
                                input,
                            });
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
                            call_id: block
                                .get("tool_use_id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string),
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
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .or_else(|| usage.get("cached_creation_input_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    (input + cached_input + cache_creation + output > 0).then_some(NormalizedEvent::TokenUsage {
        input: input
            .saturating_add(cached_input)
            .saturating_add(cache_creation),
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
    #[test]
    fn result_usage_only_adds_tokens_missing_from_messages() {
        let mut usage = MessageUsageTotals::default();
        usage.record_message(100, 20);
        usage.record_message(120, 30);
        assert_eq!(usage.result_delta(250, 60), (30, 10));
        assert_eq!(usage.result_delta(200, 40), (0, 0));
    }

    #[test]
    fn tool_calls_and_results_share_the_provider_id() {
        let call = parse_line(&json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}
            ]}
        }));
        assert!(
            matches!(&call[0], NormalizedEvent::ToolUse { call_id: Some(id), .. } if id == "toolu_1")
        );
        let result = parse_line(&json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}
            ]}
        }));
        assert!(
            matches!(&result[0], NormalizedEvent::ToolResult { call_id: Some(id), .. } if id == "toolu_1")
        );
    }
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
    fn full_access_disables_inherited_sandbox_for_start_and_resume() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Do it".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::Autonomous,
            runtime: crate::SessionRuntime::default(),
            policy: Some(crate::AgentPolicyRuntime {
                denied_tools: vec!["mcp__private__*".into()],
                ..Default::default()
            }),
            approver: None,
        };
        let prior = SessionRef {
            agent_session_id: "existing".into(),
        };
        for resume in [None, Some(&prior)] {
            let args = build_args(&spec, resume);
            assert!(args
                .windows(2)
                .any(|p| p == ["--permission-mode", "bypassPermissions"]));
            assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
            let settings = args.windows(2).find(|p| p[0] == "--settings").unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&settings[1]).unwrap(),
                json!({"sandbox": {"enabled": false}})
            );
            assert!(args
                .windows(2)
                .any(|p| p == ["--disallowedTools", "mcp__private__*"]));
            assert!(!args.iter().any(|a| a == "--permission-prompt-tool"));
            assert_eq!(
                args.windows(2).any(|p| p == ["--resume", "existing"]),
                resume.is_some()
            );
        }
        for permission in [
            PermissionPolicy::ReadOnly,
            PermissionPolicy::WorkspaceWrite,
            PermissionPolicy::Ask,
        ] {
            let restricted = SessionSpec {
                permission,
                ..spec.clone()
            };
            let args = build_args(&restricted, Some(&prior));
            assert!(!args.iter().any(|a| a == "--settings"
                || a == "--dangerously-skip-permissions"
                || a == "bypassPermissions"));
        }
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
    fn host_approval_uses_bidirectional_stream_and_manual_ask_mode() {
        let spec = SessionSpec {
            worktree: "/tmp/worktree".into(),
            prompt: "Run a command".into(),
            model: None,
            reasoning: None,
            local_model: None,
            permission: PermissionPolicy::Ask,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: Some(ApprovalResponder::new(|_| {
                Box::pin(async { ApprovalDecision::Allow })
            })),
        };
        let args = build_args(&spec, None);
        assert!(args
            .windows(2)
            .any(|p| p == ["--input-format", "stream-json"]));
        assert!(args
            .windows(2)
            .any(|p| p == ["--permission-prompt-tool", "stdio"]));
        assert!(args
            .windows(2)
            .any(|p| p == ["--permission-mode", "manual"]));
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
        assert!(args.iter().any(|arg| arg == "-p"));
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
                    "cache_creation_input_tokens": 15,
                    "output_tokens": 10
                }
            }
        });
        let events = parse_line(&v);
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::TokenUsage {
                input: 140,
                output: 10
            }]
        ));
        assert_eq!(usage_message_id(&v).as_deref(), Some("msg_1"));
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
