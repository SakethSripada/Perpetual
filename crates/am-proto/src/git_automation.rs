use serde::{Deserialize, Serialize};

/// Per-project git automation. Both off by default (opt-in): the user controls
/// whether the orchestrator commits and/or pushes after a successful run.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GitAutomation {
    /// Commit all worktree changes when a run completes.
    #[serde(default)]
    pub auto_commit: bool,
    /// Push the worktree branch to its remote after committing.
    #[serde(default)]
    pub auto_push: bool,
}
