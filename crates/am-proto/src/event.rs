use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, AgentThread, AgentThreadEvent, ApprovalDecision, ApprovalRequest,
    ApprovalResolution, CloudRun, Project, ProviderUsage, Repo, SessionEvent, Task, WorkNode,
    WorkPlanRun,
};

/// A persisted activity-log entry. Everything that happens inside a project is
/// recorded here and surfaced in the activity timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: String,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    /// Stable machine kind, e.g. `project.created`, `task.created`.
    pub kind: String,
    /// Free-form structured payload.
    pub payload: serde_json::Value,
    pub ts: DateTime<Utc>,
}

/// Input for recording an activity entry.
#[derive(Debug, Clone)]
pub struct NewActivity {
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Live application events broadcast over the in-process event bus and forwarded
/// to the UI. This is the single normalized stream the frontend subscribes to.
///
/// Session/agent variants are added in later milestones; the envelope is stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AppEvent {
    ProjectCreated(Project),
    TaskCreated(Task),
    TaskUpdated(Task),
    Activity(ActivityEvent),
    RepoConnected(Repo),
    AgentThreadCreated(AgentThread),
    AgentThreadUpdated(AgentThread),
    WorkNodeCreated(WorkNode),
    WorkNodeUpdated(WorkNode),
    WorkGraphUpdated {
        project_id: String,
    },
    WorkPlanRunUpdated(WorkPlanRun),
    /// A live event streamed from a running agent session.
    Session(SessionEvent),
    /// A live event streamed from a Workbench agent thread.
    AgentThreadEvent(AgentThreadEvent),
    /// A cloud continuation run was launched, made progress, or closed out.
    /// The full row is carried so caches can upsert without a refetch.
    CloudRunUpdated(CloudRun),
    /// An agent is waiting for the user to allow or deny an action.
    ApprovalRequested(ApprovalRequest),
    /// A sanitized provider usage update for the composer budget menu.
    ProviderUsageUpdated {
        agent: AgentKind,
        usage: ProviderUsage,
    },
    /// A pending approval was resolved (decided, cancelled, or timed out) and
    /// should be removed from the UI.
    ApprovalResolved {
        id: String,
        resolution: ApprovalResolution,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<ApprovalDecision>,
    },
}

impl AppEvent {
    /// The Tauri event channel name used for all app events.
    pub const CHANNEL: &'static str = "am://event";
}

/// An [`AppEvent`] stamped with its position in the bus's total order. `seq`
/// is monotonically increasing and gap-free, so a consumer that remembers the
/// last sequence it processed can detect missed events and request a replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: AppEvent,
}

/// Reply to an event replay request. When `complete` is false the requested
/// range fell off the retention ring; the consumer should refetch state
/// instead of trusting `events` to be contiguous with what it already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventReplay {
    pub complete: bool,
    pub latest_seq: u64,
    pub events: Vec<SequencedEvent>,
}
