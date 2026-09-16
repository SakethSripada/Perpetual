use serde::{Deserialize, Serialize};

use crate::AgentKind;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAccountAuthMode {
    #[default]
    IsolatedCli,
    OauthToken,
}

/// Non-secret account metadata. Array order is the global failover order and
/// may freely interleave Codex and Claude accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccount {
    pub id: String,
    pub label: String,
    pub agent: AgentKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether Perpetual may redeem optional usage/reset credits. Off by default.
    #[serde(default)]
    pub use_credits: bool,
    #[serde(default)]
    pub auth_mode: ProviderAccountAuthMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountStatus {
    #[serde(flatten)]
    pub account: ProviderAccount,
    pub authenticated: bool,
    /// Display-only identity reported by this isolated provider profile.
    #[serde(default)]
    pub email: Option<String>,
    pub availability: crate::AvailabilityState,
    pub reset_at: Option<chrono::DateTime<chrono::Utc>>,
    pub detail: Option<String>,
}

/// Description of a provider-owned interactive process. Environment values are
/// local-only launch data and must never be logged or sent to the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountAuthLaunch {
    pub account_id: String,
    pub label: String,
    pub agent: AgentKind,
    pub binary: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub instructions: String,
}

/// Model and reasoning defaults applied whenever Perpetual starts or switches
/// to a provider. Keeping these in the fallback policy makes provider changes
/// deterministic instead of depending on whichever CLI default happens to be
/// active on that machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTargetProfile {
    pub agent: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

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
    /// Per-provider model and reasoning choices used for new runs and automatic
    /// fallback switches. Missing entries continue to use the provider CLI's
    /// own defaults for backwards compatibility.
    #[serde(default)]
    pub agent_profiles: Vec<AgentTargetProfile>,
    /// Ordered account pool. Empty retains legacy single-login behavior.
    #[serde(default)]
    pub accounts: Vec<ProviderAccount>,
    /// When every agent is limited, resume with whichever agent's limit resets
    /// first instead of always waiting for the agent that was running.
    #[serde(default = "default_true")]
    pub resume_with_earliest: bool,
    /// When a limit reports no reset time, retry after this many seconds rather
    /// than waiting indefinitely. 0 disables the bounded retry.
    #[serde(default = "default_unknown_retry")]
    pub unknown_reset_retry_secs: u64,
    /// Compatibility field retained for clients that persist the broader
    /// Perpetual policy. It does not override the native power lifecycle
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
            agent_profiles: Vec::new(),
            accounts: Vec::new(),
            resume_with_earliest: true,
            unknown_reset_retry_secs: default_unknown_retry(),
            keep_awake: true,
        }
    }
}
