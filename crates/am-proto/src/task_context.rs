use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Agent-independent task intent that survives agent switches and session
/// resets. This is rendered into the task worktree before each run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    pub task_id: String,
    pub objective: String,
    pub requirements: String,
    pub decisions: String,
    pub progress: String,
    pub open_questions: String,
    pub next_actions: String,
    pub updated_at: DateTime<Utc>,
}

/// One archived session handoff. The rendered TASK_CONTEXT.md keeps only a
/// bounded rolling window of these; the archive is lossless and also feeds
/// blocker/sibling context packets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHandoff {
    pub id: String,
    pub task_id: String,
    pub session_id: String,
    pub agent: crate::AgentKind,
    /// Session outcome label: `completed`, `interrupted`, or `failed`.
    pub status: String,
    pub summary: String,
    pub changed_files: Vec<String>,
    pub next_actions: String,
    pub created_at: DateTime<Utc>,
}

/// Partial update payload for editable task context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContextUpdate {
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub requirements: Option<String>,
    #[serde(default)]
    pub decisions: Option<String>,
    #[serde(default)]
    pub progress: Option<String>,
    #[serde(default)]
    pub open_questions: Option<String>,
    #[serde(default)]
    pub next_actions: Option<String>,
}
