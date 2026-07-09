use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentKind, ComputeProviderKind, ModelTargetKind, TaskPriority, TaskStatus};

/// A unit of work to be completed by an agent. The primary unit of execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    /// The agent currently (or most recently) responsible for this task.
    pub primary_agent: Option<AgentKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_target: ModelTargetKind,
    #[serde(default)]
    pub compute_lease_id: Option<String>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub estimated_compute_cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_model_target: Option<ModelTargetKind>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input payload for creating a task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewTask {
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub repo_id: Option<String>,
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
    pub compute_lease_id: Option<String>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub estimated_compute_cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_model_target: Option<ModelTargetKind>,
}

/// Partial update payload for a task. `None` fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUpdate {
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
    pub model: Option<String>,
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
}
