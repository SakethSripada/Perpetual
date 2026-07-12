use serde::{Deserialize, Serialize};

/// A coding agent provider. Designed to be open-ended; new providers are added
/// here and given an [`crate::AgentKind`]-keyed adapter in `am-agents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Gemini,
    Cursor,
    OpenCode,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude_code",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Cursor => "cursor",
            AgentKind::OpenCode => "open_code",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "claude_code" => AgentKind::ClaudeCode,
            "codex" => AgentKind::Codex,
            "gemini" => AgentKind::Gemini,
            "cursor" => AgentKind::Cursor,
            "open_code" => AgentKind::OpenCode,
            _ => return None,
        })
    }

    /// Human-readable label for the UI.
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini CLI",
            AgentKind::Cursor => "Cursor",
            AgentKind::OpenCode => "OpenCode",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Lifecycle state of a task. Drives the work board and the orchestrator's
/// state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Draft,
    Queued,
    Running,
    /// Work continues on provider-hosted cloud infrastructure; no local
    /// session is active. Cleared when the cloud run is reclaimed.
    RunningInCloud,
    AwaitingApproval,
    WaitingForLimit,
    WaitingForNetwork,
    Paused,
    Review,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Draft => "draft",
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::RunningInCloud => "running_in_cloud",
            TaskStatus::AwaitingApproval => "awaiting_approval",
            TaskStatus::WaitingForLimit => "waiting_for_limit",
            TaskStatus::WaitingForNetwork => "waiting_for_network",
            TaskStatus::Paused => "paused",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => TaskStatus::Draft,
            "queued" => TaskStatus::Queued,
            "running" => TaskStatus::Running,
            "running_in_cloud" => TaskStatus::RunningInCloud,
            "awaiting_approval" => TaskStatus::AwaitingApproval,
            "waiting_for_limit" => TaskStatus::WaitingForLimit,
            "waiting_for_network" => TaskStatus::WaitingForNetwork,
            "paused" => TaskStatus::Paused,
            "review" => TaskStatus::Review,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            "cancelled" => TaskStatus::Cancelled,
            _ => return None,
        })
    }
}

/// Task priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Medium,
    High,
    Urgent,
}

impl TaskPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskPriority::Low => "low",
            TaskPriority::Medium => "medium",
            TaskPriority::High => "high",
            TaskPriority::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "low" => TaskPriority::Low,
            "medium" => TaskPriority::Medium,
            "high" => TaskPriority::High,
            "urgent" => TaskPriority::Urgent,
            _ => return None,
        })
    }
}

/// How a repository is sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    Local,
    // snake_case of `GitHub` is `git_hub`; pin the wire value to `github` so the
    // serialized form matches `as_str()`, the DB, and the TS `RepoKind` type.
    #[serde(rename = "github")]
    GitHub,
}

/// Where an agent process should execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackend {
    /// Run the provider CLI directly on the host in Perpetual's app-managed
    /// workspace. This preserves the existing behavior and is the compatibility
    /// default.
    #[default]
    Host,
    /// Run the provider through Docker's standalone `sbx` sandbox CLI.
    DockerSandbox,
    /// Run on the provider's own hosted infrastructure (Codex Cloud / Claude
    /// Code on the web). The provider is implied by the session's `AgentKind`.
    Cloud,
}

impl ExecutionBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionBackend::Host => "host",
            ExecutionBackend::DockerSandbox => "docker_sandbox",
            ExecutionBackend::Cloud => "cloud",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "host" => ExecutionBackend::Host,
            "docker_sandbox" => ExecutionBackend::DockerSandbox,
            "cloud" => ExecutionBackend::Cloud,
            _ => return None,
        })
    }
}

impl RepoKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RepoKind::Local => "local",
            RepoKind::GitHub => "github",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "local" => RepoKind::Local,
            "github" => RepoKind::GitHub,
            _ => return None,
        })
    }
}
