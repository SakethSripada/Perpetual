use serde::{Deserialize, Serialize};

use crate::AgentKind;

/// Global policy controlling what happens when an agent hits a usage limit.
///
/// Defaults are tuned to keep work flowing without stalling: switch to any
/// ready agent immediately, resume with whichever agent recovers first, and
/// bound the wait when a limit reports no reset time. `keep_awake` is retained
/// for wire compatibility; machine power behavior is controlled by the cloud
/// continuity policy in this extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitPolicy {
    /// Immediately switch to another ready agent when the active one is limited.
    #[serde(default = "default_true")]
    pub auto_switch: bool,
    /// Switch back to the preferred agent once its limit resets.
    #[serde(default = "default_true")]
    pub switch_back: bool,
    /// Preference order used when choosing a fallback / resume agent.
    #[serde(default = "default_priority")]
    pub agent_priority: Vec<AgentKind>,
    /// When every agent is limited, resume with whichever agent's limit resets
    /// first instead of always waiting for the agent that was running.
    #[serde(default = "default_true")]
    pub resume_with_earliest: bool,
    /// When a limit reports no reset time, retry after this many seconds rather
    /// than waiting indefinitely. 0 disables the bounded retry.
    #[serde(default = "default_unknown_retry")]
    pub unknown_reset_retry_secs: u64,
    /// Compatibility field retained for clients that persist the broader
    /// AgentManager policy. It does not override the native power lifecycle
    /// monitor or cloud-continuity settings.
    #[serde(default = "default_true")]
    pub keep_awake: bool,
}

fn default_true() -> bool {
    true
}

fn default_priority() -> Vec<AgentKind> {
    vec![AgentKind::ClaudeCode, AgentKind::Codex]
}

fn default_unknown_retry() -> u64 {
    600
}

impl Default for LimitPolicy {
    fn default() -> Self {
        Self {
            auto_switch: true,
            switch_back: true,
            agent_priority: default_priority(),
            resume_with_earliest: true,
            unknown_reset_retry_secs: default_unknown_retry(),
            keep_awake: true,
        }
    }
}
