use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, ApprovalDecision, ApprovalRequest, ExecutionBackend, FileChange, SessionState,
};

/// One locally authenticated agent installation advertised by a paired device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollaborationAgentCapability {
    pub agent: AgentKind,
    pub installed: bool,
    pub authenticated: bool,
    pub version: Option<String>,
}

/// A Perpetual installation paired with the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationDevice {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub extension_version: String,
    pub capabilities: Vec<CollaborationAgentCapability>,
    pub last_seen_at: DateTime<Utc>,
    pub paired_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub active_assignments: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterCollaborationDevice {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub extension_version: String,
    #[serde(default)]
    pub capabilities: Vec<CollaborationAgentCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationAssignmentStatus {
    Queued,
    Running,
    Review,
    Completed,
    Failed,
    Cancelled,
    LeaseExpired,
}

impl CollaborationAssignmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Review => "review",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::LeaseExpired => "lease_expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "review" => Self::Review,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "lease_expired" => Self::LeaseExpired,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::LeaseExpired
        )
    }
}

/// A provider turn assigned by the coordinator to one paired device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationAssignment {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub device_id: String,
    pub device_name: String,
    pub agent: AgentKind,
    pub permission: String,
    pub execution_backend: ExecutionBackend,
    /// Compact, bounded handoff prompt. Provider history is deliberately not copied.
    pub prompt: String,
    pub status: CollaborationAssignmentStatus,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollaborationAssignment {
    pub thread_id: String,
    pub device_id: String,
    pub agent: AgentKind,
    pub permission: String,
    pub execution_backend: ExecutionBackend,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
}

/// Returned only to the claiming device. The lease token is never included in lists/events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedCollaborationAssignment {
    pub assignment: CollaborationAssignment,
    pub lease_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationEventInput {
    pub assignment_id: String,
    pub lease_token: String,
    /// Stable worker-side id, used to coalesce streaming updates after retries.
    pub event_id: String,
    pub role: String,
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishCollaborationAssignment {
    pub assignment_id: String,
    pub lease_token: String,
    pub state: SessionState,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCollaborationApproval {
    pub assignment_id: String,
    pub lease_token: String,
    pub approval: ApprovalRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationApprovalDecision {
    pub id: String,
    pub local_approval_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationChangeStatus {
    Pending,
    Applied,
    AppliedWithOverwrite,
    Conflict,
    Rejected,
}

impl CollaborationChangeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::AppliedWithOverwrite => "applied_with_overwrite",
            Self::Conflict => "conflict",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "applied" => Self::Applied,
            "applied_with_overwrite" => Self::AppliedWithOverwrite,
            "conflict" => Self::Conflict,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

/// A bounded patch produced in a worker's isolated workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationChangeSet {
    pub id: String,
    pub assignment_id: String,
    pub thread_id: String,
    pub device_id: String,
    pub repo_id: String,
    pub repo_name: String,
    pub base_ref: Option<String>,
    pub files: Vec<FileChange>,
    pub patch: String,
    pub patch_sha256: String,
    pub status: CollaborationChangeStatus,
    pub conflict_files: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollaborationChangeSet {
    pub assignment_id: String,
    pub lease_token: String,
    pub repo_id: String,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub files: Vec<FileChange>,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationSnapshot {
    pub devices: Vec<CollaborationDevice>,
    pub assignments: Vec<CollaborationAssignment>,
    pub change_sets: Vec<CollaborationChangeSet>,
    pub server_time: DateTime<Utc>,
}
