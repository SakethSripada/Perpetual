use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, AgentThreadDiff, ComputeProviderKind, ExecutionBackend, ModelTargetKind, TaskDiff,
    TaskPriority, TaskStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkNodeKind {
    Group,
    Task,
    Session,
    Milestone,
}

impl WorkNodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkNodeKind::Group => "group",
            WorkNodeKind::Task => "task",
            WorkNodeKind::Session => "session",
            WorkNodeKind::Milestone => "milestone",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "group" => WorkNodeKind::Group,
            "task" => WorkNodeKind::Task,
            "session" => WorkNodeKind::Session,
            "milestone" => WorkNodeKind::Milestone,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkEdgeKind {
    DependsOn,
    Blocks,
    Handoff,
    SharesContext,
    RelatesTo,
}

impl WorkEdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkEdgeKind::DependsOn => "depends_on",
            WorkEdgeKind::Blocks => "blocks",
            WorkEdgeKind::Handoff => "handoff",
            WorkEdgeKind::SharesContext => "shares_context",
            WorkEdgeKind::RelatesTo => "relates_to",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "depends_on" => WorkEdgeKind::DependsOn,
            "blocks" => WorkEdgeKind::Blocks,
            "handoff" => WorkEdgeKind::Handoff,
            "shares_context" => WorkEdgeKind::SharesContext,
            "relates_to" => WorkEdgeKind::RelatesTo,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    PreserveManual,
    Force,
}

impl LayoutMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            LayoutMode::PreserveManual => "preserve_manual",
            LayoutMode::Force => "force",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "preserve_manual" => LayoutMode::PreserveManual,
            "force" => LayoutMode::Force,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    Manual,
    #[default]
    AutoEvaluate,
    Autonomous,
}

impl GateMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            GateMode::Manual => "manual",
            GateMode::AutoEvaluate => "auto_evaluate",
            GateMode::Autonomous => "autonomous",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => GateMode::Manual,
            "auto_evaluate" => GateMode::AutoEvaluate,
            "autonomous" => GateMode::Autonomous,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanRunState {
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl WorkPlanRunState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkPlanRunState::Running => "running",
            WorkPlanRunState::Paused => "paused",
            WorkPlanRunState::Completed => "completed",
            WorkPlanRunState::Failed => "failed",
            WorkPlanRunState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => WorkPlanRunState::Running,
            "paused" => WorkPlanRunState::Paused,
            "completed" => WorkPlanRunState::Completed,
            "failed" => WorkPlanRunState::Failed,
            "cancelled" => WorkPlanRunState::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkNode {
    pub id: String,
    pub project_id: String,
    pub parent_id: Option<String>,
    pub task_id: Option<String>,
    pub thread_id: Option<String>,
    pub kind: WorkNodeKind,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub primary_agent: Option<AgentKind>,
    pub position_x: f64,
    pub position_y: f64,
    /// Layout-engine footprint. Groups are sized from their children; `None`
    /// means "not laid out yet" and clients fall back to defaults.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// Set when a user drags the node; PreserveManual layouts anchor it.
    #[serde(default)]
    pub position_locked: bool,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewWorkNode {
    pub project_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub kind: Option<WorkNodeKind>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: TaskPriority,
    #[serde(default)]
    pub primary_agent: Option<AgentKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub compute_profile: Option<String>,
    #[serde(default)]
    pub max_compute_usd: Option<f64>,
    #[serde(default)]
    pub allow_auto_purchase: Option<bool>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub position_x: Option<f64>,
    #[serde(default)]
    pub position_y: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkNodeUpdate {
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub primary_agent: Option<AgentKind>,
    #[serde(default)]
    pub position_x: Option<f64>,
    #[serde(default)]
    pub position_y: Option<f64>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkEdge {
    pub id: String,
    pub project_id: String,
    pub source_id: String,
    pub target_id: String,
    pub kind: WorkEdgeKind,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewWorkEdge {
    pub project_id: String,
    pub source_id: String,
    pub target_id: String,
    pub kind: WorkEdgeKind,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkEdgeUpdate {
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub kind: Option<WorkEdgeKind>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkNodeRepoBinding {
    pub node_id: String,
    pub repo_id: String,
    pub repo_name: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub workspace_backend: ExecutionBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkRun {
    pub id: String,
    pub node_id: String,
    pub task_id: Option<String>,
    pub thread_id: Option<String>,
    pub agent_kind: AgentKind,
    pub run_ref: String,
    pub state: crate::SessionState,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// How a plan run reacts to a node ending up `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanFailureMode {
    /// The whole plan fails immediately (strictest; the default).
    #[default]
    Halt,
    /// The failed node's subtree is skipped; independent work keeps running.
    Continue,
    /// Failed nodes are re-queued up to `max_node_retries` times, then halt.
    Retry,
}

impl PlanFailureMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanFailureMode::Halt => "halt",
            PlanFailureMode::Continue => "continue",
            PlanFailureMode::Retry => "retry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "halt" => PlanFailureMode::Halt,
            "continue" => PlanFailureMode::Continue,
            "retry" => PlanFailureMode::Retry,
            _ => return None,
        })
    }
}

/// Optional knobs for starting a plan run; `Default` preserves the strict
/// legacy behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkPlanOptions {
    #[serde(default)]
    pub failure_mode: PlanFailureMode,
    #[serde(default)]
    pub max_node_retries: i64,
    #[serde(default)]
    pub steer_dependents_on_unblock: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_target: Option<ModelTargetKind>,
    #[serde(default)]
    pub compute_profile: Option<String>,
    #[serde(default)]
    pub max_compute_usd: Option<f64>,
    #[serde(default)]
    pub allow_auto_purchase: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkPlanRun {
    pub id: String,
    pub project_id: String,
    pub gate_mode: GateMode,
    pub state: WorkPlanRunState,
    pub max_active_runs: i64,
    #[serde(default)]
    pub failure_mode: PlanFailureMode,
    #[serde(default)]
    pub max_node_retries: i64,
    #[serde(default)]
    pub steer_dependents_on_unblock: bool,
    #[serde(default)]
    pub default_agent: Option<AgentKind>,
    #[serde(default)]
    pub default_permission: Option<String>,
    #[serde(default)]
    pub default_execution_backend: Option<ExecutionBackend>,
    #[serde(default)]
    pub evaluator_policy_json: Option<String>,
    #[serde(default)]
    pub resume_after_node_id: Option<String>,
    pub policy_envelope_id: Option<String>,
    pub total_count: i64,
    pub completed_count: i64,
    pub active_count: i64,
    pub blocked_count: i64,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedWorkMessage {
    pub node_id: String,
    pub agent_kind: AgentKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInclusion {
    pub source_kind: String,
    pub entity_id: Option<String>,
    pub title: String,
    pub snippet: String,
    pub reason: String,
    pub score: f64,
    pub bytes: i64,
    /// Rough token estimate (bytes/4 heuristic) used for token-aware
    /// budgeting. Additive for wire compatibility.
    #[serde(default)]
    pub estimated_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPacket {
    pub id: String,
    pub node_id: String,
    pub budget_bytes: i64,
    pub used_bytes: i64,
    pub summary: String,
    pub inclusions: Vec<ContextInclusion>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkGraph {
    pub project_id: String,
    pub nodes: Vec<WorkNode>,
    pub edges: Vec<WorkEdge>,
    pub repo_bindings: Vec<WorkNodeRepoBinding>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkNodeDiff {
    pub task: Option<TaskDiff>,
    pub thread: Option<AgentThreadDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationVerdict {
    #[default]
    NeedsHuman,
    Pass,
    Fail,
}

impl EvaluationVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvaluationVerdict::Pass => "pass",
            EvaluationVerdict::Fail => "fail",
            EvaluationVerdict::NeedsHuman => "needs_human",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pass" => EvaluationVerdict::Pass,
            "fail" => EvaluationVerdict::Fail,
            "needs_human" => EvaluationVerdict::NeedsHuman,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluationFollowUp {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: TaskPriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkGateEvaluation {
    pub id: String,
    pub plan_run_id: Option<String>,
    pub node_id: String,
    pub evaluator_agent: Option<AgentKind>,
    pub verdict: EvaluationVerdict,
    pub confidence: f64,
    pub findings: Vec<String>,
    pub required_follow_ups: Vec<EvaluationFollowUp>,
    pub validation_commands: Vec<String>,
    pub rationale: String,
    pub raw_output: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default = "default_evaluator_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub create_follow_up_nodes: bool,
}

impl Default for EvaluatorPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            agent: None,
            model: None,
            reasoning: None,
            timeout_secs: default_evaluator_timeout_secs(),
            create_follow_up_nodes: true,
        }
    }
}

fn default_evaluator_timeout_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCapacityPolicy {
    #[serde(default)]
    pub adaptive: bool,
    #[serde(default = "default_reserved_cpus")]
    pub reserved_cpus: usize,
    #[serde(default = "default_memory_per_session_mb")]
    pub memory_per_session_mb: u64,
    #[serde(default)]
    pub manual_max_active_sessions: Option<usize>,
    #[serde(default = "default_hard_max_active_sessions")]
    pub hard_max_active_sessions: usize,
    #[serde(default)]
    pub allow_over_recommended: bool,
}

impl Default for RunCapacityPolicy {
    fn default() -> Self {
        Self {
            adaptive: true,
            reserved_cpus: default_reserved_cpus(),
            memory_per_session_mb: default_memory_per_session_mb(),
            manual_max_active_sessions: None,
            hard_max_active_sessions: default_hard_max_active_sessions(),
            allow_over_recommended: false,
        }
    }
}

fn default_reserved_cpus() -> usize {
    1
}

fn default_memory_per_session_mb() -> u64 {
    512
}

fn default_hard_max_active_sessions() -> usize {
    512
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemCapacitySnapshot {
    pub logical_cpus: usize,
    pub total_memory_mb: u64,
    pub available_memory_mb: u64,
    pub recommended_active_sessions: usize,
    pub effective_active_sessions: usize,
    pub active_sessions: usize,
    pub queued_plan_nodes: usize,
    pub active_sandboxes: usize,
    pub warning: Option<String>,
}
