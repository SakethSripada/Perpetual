use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentKind;

/// Lifecycle of a provider-hosted cloud run (Codex Cloud task or Claude Code
/// web session) that continues an AgentManager thread while the machine is
/// unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRunStatus {
    /// Submitted; the provider is still provisioning the environment.
    Provisioning,
    /// The provider reports the task in flight (or commits are landing).
    Running,
    /// No provider status and no new commits within the stall window.
    Stalled,
    /// The provider reports the task finished.
    Completed,
    /// The provider reports the task errored.
    Failed,
    /// The provider reclaimed the environment (e.g. inactivity expiry).
    Expired,
    /// AgentManager pulled the results back and closed out the run.
    Reclaimed,
}

impl CloudRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CloudRunStatus::Provisioning => "provisioning",
            CloudRunStatus::Running => "running",
            CloudRunStatus::Stalled => "stalled",
            CloudRunStatus::Completed => "completed",
            CloudRunStatus::Failed => "failed",
            CloudRunStatus::Expired => "expired",
            CloudRunStatus::Reclaimed => "reclaimed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "provisioning" => CloudRunStatus::Provisioning,
            "running" => CloudRunStatus::Running,
            "stalled" => CloudRunStatus::Stalled,
            "completed" => CloudRunStatus::Completed,
            "failed" => CloudRunStatus::Failed,
            "expired" => CloudRunStatus::Expired,
            "reclaimed" => CloudRunStatus::Reclaimed,
            _ => return None,
        })
    }

    /// Whether the run still needs monitoring.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            CloudRunStatus::Provisioning | CloudRunStatus::Running | CloudRunStatus::Stalled
        )
    }
}

/// Why a cloud handoff was initiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudHandoffTrigger {
    /// The user asked for a cloud run from the UI.
    Manual,
    /// The machine is about to sleep.
    Sleep,
    /// The app (or machine) is shutting down.
    Shutdown,
}

impl CloudHandoffTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            CloudHandoffTrigger::Manual => "manual",
            CloudHandoffTrigger::Sleep => "sleep",
            CloudHandoffTrigger::Shutdown => "shutdown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => CloudHandoffTrigger::Manual,
            "sleep" => CloudHandoffTrigger::Sleep,
            "shutdown" => CloudHandoffTrigger::Shutdown,
            _ => return None,
        })
    }
}

/// A single cloud continuation leg of a thread. Append-only history: one row
/// per launch, closed out by `reclaimed_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRun {
    pub id: String,
    pub thread_id: String,
    pub agent_kind: AgentKind,
    /// Provider-side identifier (Codex Cloud task id / Claude session id).
    pub provider_task_id: Option<String>,
    /// Browser URL for the provider's own view of the run.
    pub url: Option<String>,
    /// Codex Cloud environment id used for the launch.
    pub env_id: Option<String>,
    /// Branch the cloud run works on (also our monitoring probe).
    pub branch: Option<String>,
    /// Worktree HEAD before the pre-launch checkpoint commit.
    pub base_commit: Option<String>,
    /// Commit the cloud run started from (after checkpoint push).
    pub launch_commit: Option<String>,
    pub status: CloudRunStatus,
    pub trigger: CloudHandoffTrigger,
    pub launched_at: DateTime<Utc>,
    /// Last time the provider reported progress or a new commit was observed.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Newest remote commit already accounted for by the monitor.
    pub last_seen_commit: Option<String>,
    pub reclaimed_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

/// Probe result for one provider's cloud execution readiness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudAvailability {
    pub agent: AgentKind,
    /// All prerequisites hold; a launch is expected to succeed.
    pub ready: bool,
    pub authenticated: bool,
    /// Human-readable blockers when not ready (missing env id, API-key auth,
    /// no GitHub remote, usage-limited, ...).
    #[serde(default)]
    pub blockers: Vec<String>,
    pub checked_at: DateTime<Utc>,
}

/// User-configurable policy for cloud continuation. Stored in `settings` like
/// [`crate::LimitPolicy`] / [`crate::LocalModelPolicy`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudPolicy {
    /// Master switch; everything below is inert when false.
    #[serde(default)]
    pub enabled: bool,
    /// Hand active runs to the cloud when the machine is about to sleep.
    #[serde(default = "default_true")]
    pub continue_on_sleep: bool,
    /// Hand active runs to the cloud when the app is quitting.
    #[serde(default = "default_true")]
    pub continue_on_shutdown: bool,
    /// When the active provider is usage-limited at handoff time, allow
    /// launching the other provider's cloud instead.
    #[serde(default)]
    pub allow_cross_provider: bool,
    /// Preference order used when cross-provider cloud handoff is allowed.
    /// The active provider still wins when it is ready; this order chooses
    /// between ready alternatives when the active provider is not available.
    #[serde(default = "default_provider_priority")]
    pub provider_priority: Vec<AgentKind>,
    /// Background checkpoint cadence while a cloud-continuable run is active,
    /// so the sleep-time delta push stays small. 0 disables checkpointing.
    #[serde(default = "default_checkpoint_interval")]
    pub checkpoint_interval_secs: u64,
    /// How often active cloud runs are polled.
    #[serde(default = "default_monitor_poll")]
    pub monitor_poll_secs: u64,
    /// Reclaim a cloud run that shows no progress for this long.
    #[serde(default = "default_stall_timeout")]
    pub stall_timeout_secs: u64,
    #[serde(default = "default_max_cloud_runs")]
    pub max_concurrent_cloud_runs: u32,
    /// Codex Cloud environment id (from chatgpt.com/codex). Required for
    /// Codex cloud launches.
    #[serde(default)]
    pub codex_env_id: Option<String>,
    /// Route every cloud launch through the in-app approval flow first.
    #[serde(default)]
    pub require_approval: bool,
}

impl Default for CloudPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            continue_on_sleep: true,
            continue_on_shutdown: true,
            allow_cross_provider: false,
            provider_priority: default_provider_priority(),
            checkpoint_interval_secs: default_checkpoint_interval(),
            monitor_poll_secs: default_monitor_poll(),
            stall_timeout_secs: default_stall_timeout(),
            max_concurrent_cloud_runs: default_max_cloud_runs(),
            codex_env_id: None,
            require_approval: false,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_provider_priority() -> Vec<AgentKind> {
    vec![AgentKind::ClaudeCode, AgentKind::Codex]
}

fn default_checkpoint_interval() -> u64 {
    120
}

fn default_monitor_poll() -> u64 {
    30
}

fn default_stall_timeout() -> u64 {
    900
}

fn default_max_cloud_runs() -> u32 {
    2
}
