use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentKind;

/// What an agent is asking permission to do. Provider-independent so the same UI
/// and round-trip serve Claude (tool gating) and Codex (exec / patch approvals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Run a shell command.
    Command,
    /// Apply a filesystem change / patch.
    FileChange,
    /// Use a named tool (Claude's generic tool-use gate).
    Tool,
}

impl ApprovalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalKind::Command => "command",
            ApprovalKind::FileChange => "file_change",
            ApprovalKind::Tool => "tool",
        }
    }
}

/// The user's decision on an [`ApprovalRequest`]. Maps onto both Claude's
/// allow/deny permission result and Codex's `ReviewDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Allow this one action.
    Allow,
    /// Allow this action and similar ones for the rest of the session.
    AllowForSession,
    /// Deny this action; the agent should continue and try something else.
    Deny,
    /// Deny and halt — the agent should stop until the user acts again.
    Abort,
}

impl ApprovalDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalDecision::Allow => "allow",
            ApprovalDecision::AllowForSession => "allow_for_session",
            ApprovalDecision::Deny => "deny",
            ApprovalDecision::Abort => "abort",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "allow" => ApprovalDecision::Allow,
            "allow_for_session" => ApprovalDecision::AllowForSession,
            "deny" => ApprovalDecision::Deny,
            "abort" => ApprovalDecision::Abort,
            _ => return None,
        })
    }

    /// Whether this decision permits the action to proceed.
    pub fn is_allow(&self) -> bool {
        matches!(
            self,
            ApprovalDecision::Allow | ApprovalDecision::AllowForSession
        )
    }
}

/// The provider/adapter-supplied portion of an approval: what the agent wants to
/// do, before the core attaches run context and an id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalAsk {
    pub kind: ApprovalKind,
    /// Tool name (Claude) or a short action label (Codex `exec`/`apply_patch`).
    pub tool_name: String,
    /// Shell command argv, when this is a command approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Working directory for the action, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Structured tool input / patch summary.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Optional model-provided reason for needing approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A pending permission request surfaced to the user, with full run context. This
/// is the payload of [`crate::AppEvent::ApprovalRequested`] and what the approval
/// UI renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Stable id the UI echoes back when resolving.
    pub id: String,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub kind: ApprovalKind,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Why a pending approval was removed, for [`crate::AppEvent::ApprovalResolved`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolution {
    /// The user chose a decision.
    Decided,
    /// The agent run ended before the user decided (auto-denied).
    Cancelled,
    /// No decision arrived before the wait deadline (auto-denied).
    TimedOut,
}

impl ApprovalResolution {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalResolution::Decided => "decided",
            ApprovalResolution::Cancelled => "cancelled",
            ApprovalResolution::TimedOut => "timed_out",
        }
    }
}
