use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentKind, FileChange, LocalModelProviderKind};

/// One model choice detected from the user's installed agent tooling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentModelOption {
    /// Raw id/alias passed to the provider CLI.
    pub id: String,
    /// Human-readable label for pickers.
    pub label: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default = "default_true")]
    pub available: bool,
    /// `codex_debug_models`, `claude_help`, `settings`, `custom`, `ollama`, ...
    pub source: String,
    #[serde(default)]
    pub reasoning: Vec<String>,
    #[serde(default)]
    pub local_provider: Option<LocalModelProviderKind>,
    #[serde(default)]
    pub local_base_url: Option<String>,
}

/// Catalog for a concrete agent install. The catalog is additive: clients can
/// ignore it and keep using [`AgentRunDefaults`] with older daemons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentModelCatalog {
    pub agent: AgentKind,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_reasoning: Option<String>,
    #[serde(default)]
    pub models: Vec<AgentModelOption>,
    #[serde(default)]
    pub reasoning: Vec<String>,
    #[serde(default)]
    pub binary_path: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub source: String,
    pub detected_at: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Result of applying a managed thread worktree back into the user's visible
/// repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentThreadApplyResult {
    pub thread_id: String,
    pub applied: bool,
    #[serde(default)]
    pub repos: Vec<AgentThreadRepoApplyResult>,
    #[serde(default)]
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentThreadRepoApplyResult {
    pub repo_id: String,
    pub repo_name: String,
    #[serde(default)]
    pub target_path: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub files: Vec<FileChange>,
    pub applied: bool,
    #[serde(default)]
    pub blocker: Option<String>,
}

fn default_true() -> bool {
    true
}
