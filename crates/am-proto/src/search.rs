use serde::{Deserialize, Serialize};

/// A single full-text search result spanning tasks, knowledge docs, and memory
/// notes. `kind` is one of `task` | `doc` | `memory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: String,
    pub entity_id: String,
    pub project_id: Option<String>,
    /// For tasks this is the task id; for task-scoped memory the owning task.
    pub task_id: Option<String>,
    pub title: String,
    /// A short highlighted excerpt around the match.
    pub snippet: String,
}
