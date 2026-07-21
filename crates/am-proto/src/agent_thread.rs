use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, ComputeProviderKind, ExecutionBackend, FileChange, LocalModelProviderKind,
    ModelTargetKind, TaskStatus,
};

/// A graceful, response-boundary usage target for a Workbench session.
/// Providers report usage after responses, so this is a target rather than an
/// exact hard token cutoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TaskBudget {
    #[default]
    Unlimited,
    Tokens {
        limit_tokens: u64,
    },
    WeeklyPercent {
        limit_percent: u8,
    },
}

impl TaskBudget {
    pub const MIN_TOKENS: u64 = 10_000;
    pub const MAX_TOKENS: u64 = 10_000_000;

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Unlimited => Ok(()),
            Self::Tokens { limit_tokens }
                if (*limit_tokens >= Self::MIN_TOKENS && *limit_tokens <= Self::MAX_TOKENS) =>
            {
                Ok(())
            }
            Self::Tokens { .. } => Err(format!(
                "Token budgets must be between {} and {} tokens.",
                Self::MIN_TOKENS,
                Self::MAX_TOKENS
            )),
            Self::WeeklyPercent { limit_percent } if (1..=100).contains(limit_percent) => Ok(()),
            Self::WeeklyPercent { .. } => {
                Err("Weekly budgets must be a whole percentage from 1% through 100%.".into())
            }
        }
    }

    pub fn is_unlimited(&self) -> bool {
        matches!(self, Self::Unlimited)
    }
}

/// A first-class Workbench conversation. It is intentionally independent from
/// tasks, while still optionally belonging to a project for repo organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentThread {
    pub id: String,
    pub project_id: Option<String>,
    pub group_id: Option<String>,
    pub title: String,
    pub status: TaskStatus,
    pub active_agent: Option<AgentKind>,
    pub preferred_agent: Option<AgentKind>,
    pub permission: String,
    pub execution_backend: ExecutionBackend,
    /// Optional model override passed to the agent CLI (e.g. `sonnet`,
    /// `opus`, `gpt-5.5`). `None` uses the CLI default.
    pub model: Option<String>,
    /// Optional reasoning-effort override (Claude `--effort`, Codex
    /// `model_reasoning_effort`). `None` uses the CLI default.
    pub reasoning: Option<String>,
    pub local_provider: Option<LocalModelProviderKind>,
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub model_target: ModelTargetKind,
    pub compute_lease_id: Option<String>,
    pub compute_provider: Option<ComputeProviderKind>,
    pub estimated_compute_cost_usd: Option<f64>,
    pub fallback_model_target: Option<ModelTargetKind>,
    pub original_agent: Option<AgentKind>,
    pub fallback_agent: Option<AgentKind>,
    pub original_model: Option<String>,
    pub fallback_model: Option<String>,
    pub original_local_provider: Option<LocalModelProviderKind>,
    pub fallback_local_provider: Option<LocalModelProviderKind>,
    pub original_local_base_url: Option<String>,
    pub fallback_local_base_url: Option<String>,
    pub switch_back_pending: bool,
    pub limit_reset_at: Option<DateTime<Utc>>,
    pub switch_back: bool,
    pub handoff_state: String,
    pub objective: String,
    pub decisions: String,
    pub progress: String,
    pub open_questions: String,
    pub next_actions: String,
    #[serde(default)]
    pub task_budget: TaskBudget,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchSessionGroup {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub color: String,
    pub collapsed: bool,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewWorkbenchSessionGroup {
    #[serde(default)]
    pub project_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkbenchSessionGroupUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub collapsed: Option<bool>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewAgentThread {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub preferred_agent: Option<AgentKind>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub execution_backend: Option<ExecutionBackend>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub local_provider: Option<LocalModelProviderKind>,
    #[serde(default)]
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub compute_lease_id: Option<String>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub estimated_compute_cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub sort_order: Option<i64>,
    #[serde(default)]
    pub task_budget: Option<TaskBudget>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentThreadUpdate {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub active_agent: Option<AgentKind>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub preferred_agent: Option<AgentKind>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub execution_backend: Option<ExecutionBackend>,
    /// `Some("")` clears the override (use CLI default); `Some(value)` sets it.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub local_provider: Option<LocalModelProviderKind>,
    #[serde(default)]
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub compute_lease_id: Option<String>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub estimated_compute_cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub decisions: Option<String>,
    #[serde(default)]
    pub progress: Option<String>,
    #[serde(default)]
    pub open_questions: Option<String>,
    #[serde(default)]
    pub next_actions: Option<String>,
    #[serde(default)]
    pub task_budget: Option<TaskBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentThreadRepo {
    pub thread_id: String,
    pub repo_id: String,
    pub repo_name: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub workspace_backend: ExecutionBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurn {
    pub id: String,
    pub thread_id: String,
    pub agent_kind: AgentKind,
    pub agent_session_id: Option<String>,
    pub state: crate::SessionState,
    pub permission: String,
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
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentThreadEvent {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub role: String,
    pub kind: String,
    pub text: Option<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTurn {
    pub id: String,
    pub thread_id: String,
    pub agent_kind: AgentKind,
    pub permission: String,
    pub message: String,
    #[serde(default = "default_echo_user_message")]
    pub echo_user_message: bool,
    #[serde(default)]
    pub client_message_id: Option<String>,
    pub policy_envelope_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn default_echo_user_message() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentThreadDiff {
    pub repos: Vec<AgentThreadRepoDiff>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentThreadRepoDiff {
    pub repo_id: String,
    pub repo_name: String,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub worktree_path: Option<String>,
    pub files: Vec<FileChange>,
    pub patch: String,
}
