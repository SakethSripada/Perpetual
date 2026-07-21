//! Agent adapter layer — the heart of Perpetual.
//!
//! Every coding agent (Claude Code, Codex, …) is driven as a subprocess in
//! headless/streaming mode and normalized into a single [`NormalizedEvent`]
//! stream behind the [`AgentAdapter`] trait. This normalization is what makes
//! unified activity, agent switching, and usage-awareness possible.
//!
//! Security: subprocesses are spawned with argument arrays only (never a shell),
//! placed in their own process group so the whole tree is killed on stop, and
//! `kill_on_drop` guards against orphans. Event channels are bounded for
//! backpressure.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use am_proto::{ApprovalAsk, ApprovalDecision, ApprovalKind};

pub use am_proto::AgentKind;
pub use am_proto::ExecutionBackend;

mod detect;
mod limits;
mod network;
mod process;
mod runtime;

pub mod claude;
pub mod cloud;
pub mod codex;
mod codex_app_server;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use detect::{binary_version, find_binary};
pub use network::detect_network_error;
pub use process::ManagedChild;
pub use runtime::{
    cleanup_sandbox, forget_sandbox, spawn_for_runtime, spawn_for_runtime_with_env,
    spawn_host_piped_stdin, RuntimeLimits, SessionRuntime,
};

/// Errors from agent adapters.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent binary not found: {0}")]
    NotInstalled(AgentKind),
    #[error("agent not authenticated: {0:?}")]
    Unauthenticated(AgentKind),
    #[error("failed to spawn agent process: {0}")]
    Spawn(String),
    #[error("protocol/parse error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

/// Permission posture for a run. The conservative default ([`Self::WorkspaceWrite`])
/// lets the agent edit files but aborts on risky shell/network; [`Self::Autonomous`]
/// is an explicit opt-in for full automation inside the isolated worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    /// The agent runs freely but must ask the user (live, in-app) before each
    /// gated action; the user allows or denies mid-run.
    Ask,
    Autonomous,
}

/// Future returned by an [`ApprovalResponder`].
pub type ApprovalFuture = Pin<Box<dyn Future<Output = ApprovalDecision> + Send>>;

/// A per-run callback an adapter invokes to ask the user to allow or deny an
/// action. `am-core` supplies one that publishes an approval event and awaits the
/// user's decision; adapters that support live approval (Codex app-server) call
/// it. Wrapped so [`SessionSpec`] keeps its `Debug`/`Clone` derives.
#[derive(Clone)]
pub struct ApprovalResponder(Arc<dyn Fn(ApprovalAsk) -> ApprovalFuture + Send + Sync>);

impl ApprovalResponder {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(ApprovalAsk) -> ApprovalFuture + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Ask the user; resolves to their decision (or a default-deny on timeout /
    /// run end, decided by the responder implementation).
    pub async fn ask(&self, ask: ApprovalAsk) -> ApprovalDecision {
        (self.0)(ask).await
    }
}

impl std::fmt::Debug for ApprovalResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalResponder(..)")
    }
}

/// How to launch (or continue) an agent session.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Process working directory — the task's isolated worktree.
    pub worktree: PathBuf,
    /// Instruction / continuation prompt.
    pub prompt: String,
    /// Optional explicit model override.
    pub model: Option<String>,
    /// Optional reasoning-effort override (Claude `--effort`, Codex
    /// `model_reasoning_effort`).
    pub reasoning: Option<String>,
    /// Optional local model target. V1 local execution is Codex OSS only; other
    /// adapters ignore this field.
    pub local_model: Option<LocalModelRuntime>,
    pub permission: PermissionPolicy,
    pub runtime: SessionRuntime,
    /// Effective policy controls derived by `am-core` for this single launch.
    /// Adapters translate these into the native CLI/config surfaces they own.
    pub policy: Option<AgentPolicyRuntime>,
    /// Live-approval callback for [`PermissionPolicy::Ask`]. Set by `am-core`;
    /// only adapters with a bidirectional protocol (Codex app-server) use it.
    /// `None` for adapters that do not support live approval callbacks.
    pub approver: Option<ApprovalResponder>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentPolicyRuntime {
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub allowed_mcp_servers: Vec<String>,
    pub denied_mcp_servers: Vec<String>,
    pub denied_context_globs: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub disable_remote_mcp_connectors: bool,
    pub max_budget_usd: Option<f64>,
    /// Private launch metadata used by the graceful session budget adapter.
    /// It is never serialized into the webview or transcript.
    pub task_budget: Option<am_proto::TaskBudget>,
}

/// Local provider target passed to adapters that can run against local models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModelRuntime {
    pub provider: am_proto::LocalModelProviderKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_token: Option<String>,
}

/// Timeouts and termination grace for a launched agent process.
#[derive(Debug, Clone, Copy)]
pub struct SessionTimeouts {
    pub run_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub stop_grace_secs: u64,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            run_timeout_secs: 7_200,
            idle_timeout_secs: 900,
            stop_grace_secs: 30,
        }
    }
}

/// Reference to a prior provider session, used to resume.
#[derive(Debug, Clone)]
pub struct SessionRef {
    pub agent_session_id: String,
}

/// Kind of filesystem change observed during a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

/// Terminal status of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Completed,
    Interrupted,
    Failed,
}

/// The normalized event model. Every adapter parses its provider's stream into
/// this shape; the rest of the system only ever sees these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedEvent {
    SessionStarted {
        session_id: String,
    },
    AssistantText {
        text: String,
    },
    /// An incremental assistant-text fragment. Consumers that persist a
    /// transcript should coalesce consecutive deltas into one message and use
    /// the following `AssistantText` event as the authoritative final value.
    AssistantTextDelta {
        delta: String,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        ok: bool,
        summary: String,
    },
    FileChanged {
        path: PathBuf,
        change: ChangeKind,
    },
    TokenUsage {
        input: u64,
        output: u64,
    },
    /// Internal account-level quota sample used for weekly task budgets. Core
    /// consumes it without persisting or publishing the raw values.
    QuotaWindow {
        used_percent: f64,
        reset_at: Option<DateTime<Utc>>,
    },
    AwaitingApproval {
        detail: String,
    },
    /// The provider reported a usage/rate limit; `reset_at` powers
    /// auto-continuation and agent switching.
    UsageLimitReached {
        reset_at: Option<DateTime<Utc>>,
    },
    /// The provider appears unreachable because of DNS/connectivity/timeout
    /// failure. Core uses this to pause cleanly or move to local fallback.
    NetworkUnavailable {
        message: String,
    },
    Error {
        message: String,
        retryable: bool,
    },
    SessionEnded {
        status: SessionStatus,
    },
}

/// Install / auth status of an agent on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstallStatus {
    pub kind: AgentKind,
    pub installed: bool,
    pub authenticated: bool,
    pub version: Option<String>,
    pub binary_path: Option<PathBuf>,
}

/// Control surface for a running session.
pub struct SessionControl {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    steer: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl SessionControl {
    pub fn new(cancel: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            cancel: Some(cancel),
            steer: None,
        }
    }

    pub fn with_steer(
        cancel: tokio::sync::oneshot::Sender<()>,
        steer: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            cancel: Some(cancel),
            steer: Some(steer),
        }
    }

    /// Request graceful cancellation; the driver kills the process group.
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }

    pub fn steer(&self, instruction: String) -> bool {
        self.steer
            .as_ref()
            .is_some_and(|tx| tx.send(instruction).is_ok())
    }
}

/// A live session: a bounded normalized event stream plus controls over the
/// underlying process. The provider's resumable session id arrives as the first
/// [`NormalizedEvent::SessionStarted`] event (not known until the process starts).
pub struct SessionHandle {
    pub events: mpsc::Receiver<NormalizedEvent>,
    pub control: SessionControl,
}

/// The contract every coding agent implements.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// Probe install/auth/version (resolving the binary from standard install
    /// locations, not just `$PATH`).
    async fn detect(&self) -> AgentInstallStatus;

    /// Start a fresh session.
    async fn start(&self, spec: SessionSpec) -> Result<SessionHandle, AgentError>;

    /// Resume the provider's prior session, continuing in the same worktree.
    async fn resume(
        &self,
        prior: SessionRef,
        spec: SessionSpec,
    ) -> Result<SessionHandle, AgentError>;
}
