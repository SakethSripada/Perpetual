use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A short, durable memory note. Project-scoped when `task_id` is `None`,
/// task-scoped otherwise. Captures facts agents and the team should remember
/// (decisions, gotchas, conventions) that survive across tasks and agent
/// switches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input payload for creating a memory note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMemoryNote {
    pub project_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub body: String,
}

/// Partial update payload for a memory note.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryNoteUpdate {
    #[serde(default)]
    pub body: Option<String>,
}
