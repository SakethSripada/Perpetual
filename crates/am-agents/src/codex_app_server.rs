//! Codex app-server transport for live approval ([`PermissionPolicy::Ask`]).
//!
//! `codex exec` is non-interactive and cannot pause for approval, so Ask-mode
//! Codex runs are driven over `codex app-server`'s JSON-RPC stdio protocol
//! instead. We initialize, start a thread, and start one turn; the server's v2
//! `thread/turn/item` notifications are mapped into [`NormalizedEvent`], and its
//! `requestApproval` server→client requests are routed to the user through the
//! [`ApprovalResponder`], whose decision is sent back as a `ReviewDecision`.
//!
//! Framing is newline-delimited JSON. Requests carry an `id`; notifications do
//! not; server→client requests carry both `id` and `method`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::process::ManagedChild;
use crate::runtime::spawn_host_piped_stdin;
use crate::{
    ApprovalAsk, ApprovalDecision, ApprovalKind, ApprovalResponder, ChangeKind, NormalizedEvent,
    PermissionPolicy, SessionControl, SessionHandle, SessionRef, SessionSpec, SessionStatus,
};

const BIN: &str = "codex";
const CHANNEL_CAPACITY: usize = 256;
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

type PendingResponses = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Launch (or resume) a Codex Ask-mode run over the app-server transport.
pub(crate) async fn launch(
    spec: SessionSpec,
    resume: Option<SessionRef>,
    approver: ApprovalResponder,
) -> Result<SessionHandle, AgentLaunchError> {
    let envs = crate::codex::session_env(&spec);
    let mut child = spawn_host_piped_stdin(BIN, &["app-server".to_string()], &spec.worktree, &envs)
        .await
        .map_err(AgentLaunchError::Spawn)?;
    let stdin = child
        .take_stdin()
        .ok_or_else(|| AgentLaunchError::other("no stdin pipe"))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| AgentLaunchError::other("no stdout pipe"))?;
    let stderr = child.take_stderr();

    let (events_tx, events_rx) = mpsc::channel::<NormalizedEvent>(CHANNEL_CAPACITY);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    let limits = spec.runtime.limits();
    tokio::spawn(drive(
        child, stdin, stdout, stderr, spec, resume, approver, events_tx, cancel_rx, limits,
    ));

    Ok(SessionHandle {
        events: events_rx,
        control: SessionControl::new(cancel_tx),
    })
}

/// Minimal launch error so the adapter can fall back to `codex exec`.
pub(crate) enum AgentLaunchError {
    Spawn(crate::AgentError),
    Other(String),
}

impl AgentLaunchError {
    fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub(crate) fn into_message(self) -> String {
        match self {
            AgentLaunchError::Spawn(err) => err.to_string(),
            AgentLaunchError::Other(msg) => msg,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    mut child: ManagedChild,
    stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    spec: SessionSpec,
    resume: Option<SessionRef>,
    approver: ApprovalResponder,
    events_tx: mpsc::Sender<NormalizedEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
    limits: crate::runtime::RuntimeLimits,
) {
    // Drain stderr so the pipe never blocks the child.
    let stderr_task = stderr.map(|se| {
        tokio::spawn(async move {
            let mut lines = BufReader::new(se).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(line = %line, "codex app-server stderr");
            }
        })
    });

    // Single writer owns stdin; the orchestrator and approval tasks enqueue lines.
    let (out_tx, out_rx) = mpsc::channel::<String>(64);
    let writer_task = tokio::spawn(writer(stdin, out_rx));

    let pending: PendingResponses = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));
    let (terminal_tx, terminal_rx) = oneshot::channel::<SessionStatus>();

    // Reader dispatches responses, notifications, and server→client requests.
    let reader_task = tokio::spawn(reader(
        stdout,
        pending.clone(),
        events_tx.clone(),
        out_tx.clone(),
        approver,
        terminal_tx,
    ));

    let rpc = Rpc {
        out_tx: out_tx.clone(),
        pending: pending.clone(),
        next_id: next_id.clone(),
    };

    let final_status = match run_turn(&rpc, &spec, resume.as_ref(), &events_tx).await {
        Ok(turn_id) => {
            // Wait for the turn to complete, the user to cancel, or a timeout.
            let hard = tokio::time::sleep(limits.run_timeout);
            tokio::pin!(hard);
            tokio::select! {
                status = terminal_rx => status.unwrap_or(SessionStatus::Failed),
                _ = &mut cancel_rx => {
                    rpc.notify_or_request_interrupt(&turn_id).await;
                    SessionStatus::Interrupted
                }
                _ = &mut hard => {
                    let _ = events_tx
                        .send(NormalizedEvent::Error {
                            message: "agent run timed out".into(),
                            retryable: true,
                        })
                        .await;
                    SessionStatus::Interrupted
                }
            }
        }
        Err(message) => {
            let _ = events_tx
                .send(NormalizedEvent::Error {
                    message,
                    retryable: false,
                })
                .await;
            SessionStatus::Failed
        }
    };

    // Tear the child down and finish the stream.
    child.terminate_group();
    if tokio::time::timeout(TERMINATE_GRACE, child.wait())
        .await
        .is_err()
    {
        child.kill_group();
    }
    let _ = events_tx
        .send(NormalizedEvent::SessionEnded {
            status: final_status,
        })
        .await;

    reader_task.abort();
    writer_task.abort();
    if let Some(task) = stderr_task {
        task.abort();
    }
}

/// Run the initialize → thread/start → turn/start sequence. Returns the turn id.
async fn run_turn(
    rpc: &Rpc,
    spec: &SessionSpec,
    resume: Option<&SessionRef>,
    events_tx: &mpsc::Sender<NormalizedEvent>,
) -> Result<String, String> {
    rpc.request(
        "initialize",
        json!({
            "clientInfo": { "name": "AgentManager", "version": env!("CARGO_PKG_VERSION") }
        }),
    )
    .await?;
    rpc.notify("initialized", json!({})).await;

    // Reuse the prior Codex thread when resuming so its context carries over;
    // fall back to a fresh thread if resume is unavailable.
    let thread_value = match resume {
        Some(prior) => {
            match rpc
                .request(
                    "thread/resume",
                    json!({ "threadId": prior.agent_session_id }),
                )
                .await
            {
                Ok(value) => value,
                Err(_) => {
                    rpc.request("thread/start", thread_start_params(spec))
                        .await?
                }
            }
        }
        None => {
            rpc.request("thread/start", thread_start_params(spec))
                .await?
        }
    };

    let thread_id = thread_value
        .pointer("/thread/id")
        .or_else(|| thread_value.get("threadId"))
        .and_then(Value::as_str)
        .ok_or_else(|| "codex app-server returned no thread id".to_string())?
        .to_string();
    let _ = events_tx
        .send(NormalizedEvent::SessionStarted {
            session_id: thread_id.clone(),
        })
        .await;

    let turn = rpc
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": spec.prompt }],
                "approvalPolicy": approval_policy(spec.permission),
            }),
        )
        .await?;
    let turn_id = turn
        .pointer("/turn/id")
        .or_else(|| turn.get("turnId"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(turn_id)
}

/// Codex approval policy for a permission level. `Ask` prompts for every action
/// (`untrusted`); `WorkspaceWrite`/Edit lets the sandbox handle workspace edits
/// and only prompts when Codex needs to escalate (`on-request`).
fn approval_policy(permission: PermissionPolicy) -> &'static str {
    match permission {
        PermissionPolicy::Ask => "untrusted",
        _ => "on-request",
    }
}

fn thread_start_params(spec: &SessionSpec) -> Value {
    let mut params = json!({
        "cwd": spec.worktree.to_string_lossy(),
        "approvalPolicy": approval_policy(spec.permission),
        "sandbox": "workspace-write",
    });
    if let Some(model) = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty() && !matches!(*m, "default" | "auto"))
    {
        params["model"] = json!(model);
    }
    params
}

/// JSON-RPC request/notification sender over the shared writer.
#[derive(Clone)]
struct Rpc {
    out_tx: mpsc::Sender<String>,
    pending: PendingResponses,
    next_id: Arc<AtomicU64>,
}

impl Rpc {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let line = json!({ "id": id, "method": method, "params": params }).to_string();
        if self.out_tx.send(line).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err("codex app-server stdin closed".into());
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err("codex app-server closed before responding".into()),
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let line = json!({ "method": method, "params": params }).to_string();
        let _ = self.out_tx.send(line).await;
    }

    async fn notify_or_request_interrupt(&self, turn_id: &str) {
        // Best-effort interrupt; the child is killed regardless.
        self.notify("turn/interrupt", json!({ "turnId": turn_id }))
            .await;
    }
}

async fn writer(mut stdin: ChildStdin, mut out_rx: mpsc::Receiver<String>) {
    while let Some(mut line) = out_rx.recv().await {
        line.push('\n');
        if stdin.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdin.flush().await;
    }
}

async fn reader(
    stdout: tokio::process::ChildStdout,
    pending: PendingResponses,
    events_tx: mpsc::Sender<NormalizedEvent>,
    out_tx: mpsc::Sender<String>,
    approver: ApprovalResponder,
    terminal_tx: oneshot::Sender<SessionStatus>,
) {
    let mut terminal_tx = Some(terminal_tx);
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            tracing::debug!(line = %trimmed, "ignoring non-json app-server line");
            continue;
        };

        let has_id = value.get("id").is_some();
        let method = value.get("method").and_then(Value::as_str);

        match (has_id, method) {
            // Response to one of our requests.
            (true, None) => {
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let result = if let Some(err) = value.get("error") {
                            Err(err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("app-server error")
                                .to_string())
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = tx.send(result);
                    }
                }
            }
            // Server→client request (approvals, etc.).
            (true, Some(method)) => {
                handle_server_request(method, &value, &approver, &out_tx).await;
            }
            // Notification.
            (false, Some(method)) => {
                if let Some(status) = handle_notification(method, &value, &events_tx).await {
                    if let Some(tx) = terminal_tx.take() {
                        let _ = tx.send(status);
                    }
                }
            }
            _ => {}
        }
    }
    // Stream closed without a terminal turn notification.
    if let Some(tx) = terminal_tx.take() {
        let _ = tx.send(SessionStatus::Completed);
    }
}

/// Handle a server→client request. Approval requests are routed to the user; all
/// others are acknowledged so the server is never left waiting.
async fn handle_server_request(
    method: &str,
    value: &Value,
    approver: &ApprovalResponder,
    out_tx: &mpsc::Sender<String>,
) {
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    if let Some(ask) = approval_ask_for(method, &params) {
        let approver = approver.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let decision = approver.ask(ask).await;
            let reply = json!({
                "id": id,
                "result": { "decision": review_decision(decision) }
            });
            let _ = out_tx.send(reply.to_string()).await;
        });
    } else {
        // Unknown request: reply with an empty result so Codex can proceed.
        let reply = json!({ "id": id, "result": {} });
        let _ = out_tx.send(reply.to_string()).await;
    }
}

/// Build an [`ApprovalAsk`] from a Codex approval request, or `None` if the
/// method is not an approval request.
fn approval_ask_for(method: &str, params: &Value) -> Option<ApprovalAsk> {
    match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.as_str().map(ToString::to_string))
                        .collect::<Vec<_>>()
                })
                .filter(|c| !c.is_empty());
            Some(ApprovalAsk {
                kind: ApprovalKind::Command,
                tool_name: "command".to_string(),
                command,
                cwd: params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                input: params.clone(),
                reason: params
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => Some(ApprovalAsk {
            kind: ApprovalKind::FileChange,
            tool_name: "apply_patch".to_string(),
            command: None,
            cwd: params
                .get("cwd")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            input: params.clone(),
            reason: params
                .get("reason")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }),
        _ => None,
    }
}

/// Map our decision onto Codex's `ReviewDecision`.
fn review_decision(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Allow => "approved",
        ApprovalDecision::AllowForSession => "approved_for_session",
        ApprovalDecision::Deny => "denied",
        ApprovalDecision::Abort => "abort",
    }
}

/// Map a v2 notification into normalized events. Returns a terminal status when
/// the turn ends.
async fn handle_notification(
    method: &str,
    value: &Value,
    events_tx: &mpsc::Sender<NormalizedEvent>,
) -> Option<SessionStatus> {
    let params = value.get("params").unwrap_or(value);
    match method {
        "turn/completed" => {
            if let Some(usage) = parse_usage(params.pointer("/turn/usage")) {
                let _ = events_tx.send(usage).await;
            }
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .map(turn_status)
                .unwrap_or(SessionStatus::Completed);
            if status == SessionStatus::Failed {
                if let Some(message) = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                {
                    send_error_or_limit(events_tx, message).await;
                }
            }
            Some(status)
        }
        "turn/failed" => {
            if let Some(message) = params.pointer("/error/message").and_then(Value::as_str) {
                send_error_or_limit(events_tx, message).await;
            }
            Some(SessionStatus::Failed)
        }
        "error" => {
            if let Some(message) = params.get("message").and_then(Value::as_str) {
                send_error_or_limit(events_tx, message).await;
            }
            None
        }
        "item/started" | "item/completed" => {
            let completed = method == "item/completed";
            if let Some(item) = params.get("item") {
                for event in map_item(item, completed) {
                    let _ = events_tx.send(event).await;
                }
            }
            None
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    let _ = events_tx
                        .send(NormalizedEvent::AssistantTextDelta {
                            delta: delta.to_string(),
                        })
                        .await;
                }
            }
            None
        }
        _ => None,
    }
}

/// Send an error event, upgrading recognized usage-limit / network messages to
/// their dedicated events (matching the `codex exec` path) so the orchestrator's
/// limit/network handling fires for app-server runs too.
async fn send_error_or_limit(events_tx: &mpsc::Sender<NormalizedEvent>, message: &str) {
    let mut events = Vec::new();
    crate::codex::push_error_or_limit(&mut events, message);
    for event in events {
        let _ = events_tx.send(event).await;
    }
}

fn turn_status(status: &str) -> SessionStatus {
    match status {
        "completed" => SessionStatus::Completed,
        "interrupted" => SessionStatus::Interrupted,
        "failed" => SessionStatus::Failed,
        _ => SessionStatus::Completed,
    }
}

/// Map a v2 `ThreadItem` into normalized events.
fn map_item(item: &Value, completed: bool) -> Vec<NormalizedEvent> {
    let mut out = Vec::new();
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") if completed => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    out.push(NormalizedEvent::AssistantText {
                        text: text.to_string(),
                    });
                }
            }
        }
        Some("commandExecution") => {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if completed {
                let status = item.get("status").and_then(Value::as_str).unwrap_or("");
                let exit = item.get("exitCode").and_then(Value::as_i64);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let summary = if !output.trim().is_empty() {
                    truncate(output, 800)
                } else if let Some(code) = exit {
                    format!("exit code {code}")
                } else {
                    status.to_string()
                };
                out.push(NormalizedEvent::ToolResult {
                    ok: status == "completed" && exit.unwrap_or(0) == 0,
                    summary,
                });
            } else {
                out.push(NormalizedEvent::ToolUse {
                    name: "Command".to_string(),
                    input: json!({ "command": command }),
                });
            }
        }
        Some("fileChange") if completed => {
            if let Some(changes) = item.get("changes").and_then(Value::as_array) {
                for change in changes {
                    let Some(path) = change.get("path").and_then(Value::as_str) else {
                        continue;
                    };
                    let kind = match change.get("kind").and_then(Value::as_str) {
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
                    ok: item.get("status").and_then(Value::as_str) == Some("completed"),
                    summary: format!(
                        "{} file change{}",
                        changes.len(),
                        if changes.len() == 1 { "" } else { "s" }
                    ),
                });
            }
        }
        Some("mcpToolCall") => {
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let server = item.get("server").and_then(Value::as_str).unwrap_or("");
            let name = if server.is_empty() {
                tool.to_string()
            } else {
                format!("{server}/{tool}")
            };
            if completed {
                let ok = item.get("status").and_then(Value::as_str) == Some("completed")
                    && item.get("error").is_none();
                out.push(NormalizedEvent::ToolResult {
                    ok,
                    summary: item
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| name.clone()),
                });
            } else {
                out.push(NormalizedEvent::ToolUse {
                    name,
                    input: item.get("arguments").cloned().unwrap_or(Value::Null),
                });
            }
        }
        _ => {}
    }
    out
}

fn parse_usage(usage: Option<&Value>) -> Option<NormalizedEvent> {
    let usage = usage?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("inputTokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as u64;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("outputTokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as u64;
    (input + output > 0).then_some(NormalizedEvent::TokenUsage { input, output })
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

    #[test]
    fn approval_policy_per_permission() {
        assert_eq!(approval_policy(PermissionPolicy::Ask), "untrusted");
        assert_eq!(
            approval_policy(PermissionPolicy::WorkspaceWrite),
            "on-request"
        );
    }

    #[test]
    fn thread_start_uses_untrusted_for_ask() {
        let spec = SessionSpec {
            worktree: "/tmp/wt".into(),
            prompt: "go".into(),
            model: None,
            reasoning: None,
            local_model: None,
            mcp: None,
            permission: PermissionPolicy::Ask,
            runtime: crate::SessionRuntime::default(),
            policy: None,
            approver: None,
        };
        assert_eq!(thread_start_params(&spec)["approvalPolicy"], "untrusted");
        let edit = SessionSpec {
            permission: PermissionPolicy::WorkspaceWrite,
            ..spec
        };
        assert_eq!(thread_start_params(&edit)["approvalPolicy"], "on-request");
    }

    #[test]
    fn maps_decisions_to_review_decision() {
        assert_eq!(review_decision(ApprovalDecision::Allow), "approved");
        assert_eq!(
            review_decision(ApprovalDecision::AllowForSession),
            "approved_for_session"
        );
        assert_eq!(review_decision(ApprovalDecision::Deny), "denied");
        assert_eq!(review_decision(ApprovalDecision::Abort), "abort");
    }

    #[test]
    fn builds_command_approval_ask() {
        let params = json!({
            "callId": "c1",
            "command": ["bash", "-lc", "rm -rf build"],
            "cwd": "/work",
            "reason": "destructive"
        });
        let ask = approval_ask_for("item/commandExecution/requestApproval", &params).unwrap();
        assert!(matches!(ask.kind, ApprovalKind::Command));
        assert_eq!(ask.command.as_deref().unwrap().len(), 3);
        assert_eq!(ask.cwd.as_deref(), Some("/work"));
        assert_eq!(ask.reason.as_deref(), Some("destructive"));
    }

    #[test]
    fn non_approval_request_is_ignored() {
        assert!(approval_ask_for("item/tool/call", &json!({})).is_none());
    }

    #[test]
    fn maps_agent_message_item() {
        let item = json!({ "type": "agentMessage", "id": "i1", "text": "Done." });
        let events = map_item(&item, true);
        assert!(matches!(&events[0], NormalizedEvent::AssistantText { text } if text == "Done."));
    }

    #[tokio::test]
    async fn maps_agent_message_delta_notification() {
        let (tx, mut rx) = mpsc::channel(2);
        let status = handle_notification(
            "item/agentMessage/delta",
            &json!({
                "params": {
                    "threadId": "t1",
                    "turnId": "turn1",
                    "itemId": "i1",
                    "delta": "Smooth"
                }
            }),
            &tx,
        )
        .await;
        assert!(status.is_none());
        assert!(matches!(
            rx.recv().await,
            Some(NormalizedEvent::AssistantTextDelta { delta }) if delta == "Smooth"
        ));
    }

    #[test]
    fn maps_command_lifecycle() {
        let started = map_item(
            &json!({ "type": "commandExecution", "id": "i", "command": "cargo test", "status": "inProgress" }),
            false,
        );
        assert!(matches!(&started[0], NormalizedEvent::ToolUse { name, .. } if name == "Command"));
        let done = map_item(
            &json!({ "type": "commandExecution", "id": "i", "command": "cargo test", "aggregatedOutput": "ok\n", "exitCode": 0, "status": "completed" }),
            true,
        );
        assert!(matches!(
            &done[0],
            NormalizedEvent::ToolResult { ok: true, .. }
        ));
    }
}
