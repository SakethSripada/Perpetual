use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, ComputeProviderKind, ExecutionBackend, LocalModelProviderKind, ModelTargetKind,
};

/// Lifecycle of an agent session (a single run against a task).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Completed,
    Interrupted,
    Failed,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Running => "running",
            SessionState::Completed => "completed",
            SessionState::Interrupted => "interrupted",
            SessionState::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => SessionState::Running,
            "completed" => SessionState::Completed,
            "interrupted" => SessionState::Interrupted,
            "failed" => SessionState::Failed,
            _ => return None,
        })
    }
}

/// A persisted agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub task_id: String,
    pub agent_kind: AgentKind,
    /// The provider's own resumable session id (captured from its init event).
    pub agent_session_id: Option<String>,
    pub execution_backend: ExecutionBackend,
    pub sandbox_name: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub local_provider: Option<LocalModelProviderKind>,
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub model_target: ModelTargetKind,
    pub compute_lease_id: Option<String>,
    pub compute_provider: Option<ComputeProviderKind>,
    pub estimated_compute_cost_usd: Option<f64>,
    pub fallback_model_target: Option<ModelTargetKind>,
    pub target_hash: Option<String>,
    pub policy_envelope_id: Option<String>,
    pub state: SessionState,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// A single normalized, UI-facing event from a session. This is the stable shape
/// the frontend renders and the transcript stores; the richer adapter-internal
/// `NormalizedEvent` is mapped into this in `am-core`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: String,
    pub session_id: String,
    pub task_id: String,
    /// `system` | `assistant` | `tool` | `app`.
    pub role: String,
    /// Variant tag: `session_started`, `assistant_text`, `tool_use`,
    /// `tool_result`, `file_changed`, `usage_limit`, `error`, `session_ended`,
    /// `status`.
    pub kind: String,
    /// Primary display text.
    pub text: Option<String>,
    /// Structured extras (tool input, reset time, status, …).
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

/// A single changed file in a task's worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    /// `added` | `modified` | `deleted` | `renamed` | `untracked`.
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
}

/// The diff of a task's worktree against its base.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDiff {
    pub files: Vec<FileChange>,
    /// Unified diff text (may be empty when there are no changes).
    pub patch: String,
    pub repo_id: Option<String>,
    pub repo_name: Option<String>,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub worktree_path: Option<String>,
}

/// Whether an agent can currently accept more work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    Unknown,
    Available,
    Limited,
}

impl AvailabilityState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AvailabilityState::Unknown => "unknown",
            AvailabilityState::Available => "available",
            AvailabilityState::Limited => "limited",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unknown" => AvailabilityState::Unknown,
            "available" => AvailabilityState::Available,
            "limited" => AvailabilityState::Limited,
            _ => return None,
        })
    }
}

/// Install / auth status of an agent, for the Agents settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageWindow {
    pub used_percent: f64,
    pub reset_at: Option<DateTime<Utc>>,
}

/// Sanitized provider usage summary. This is intentionally limited to the
/// user-facing percentage and reset time; raw provider payloads and token
/// streams stay internal to the agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub five_hour: Option<ProviderUsageWindow>,
    pub weekly: Option<ProviderUsageWindow>,
}

/// Install / auth status of an agent, for the Agents settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub kind: AgentKind,
    pub installed: bool,
    pub authenticated: bool,
    pub version: Option<String>,
    pub binary_path: Option<String>,
    pub availability: AvailabilityState,
    pub reset_at: Option<DateTime<Utc>>,
    pub last_checked: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage: Option<ProviderUsage>,
}

/// Model/reasoning defaults read from the agent's own local CLI/IDE config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunDefaults {
    pub kind: AgentKind,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}
