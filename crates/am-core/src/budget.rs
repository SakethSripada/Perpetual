use am_proto::TaskBudget;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct EnforcementState {
    pub(crate) weekly_baseline_percent: Option<f64>,
    pub(crate) weekly_consumed_percent: f64,
    pub(crate) reminder_sent: bool,
    pub(crate) closeout_sent: bool,
    pub(crate) provider: Option<String>,
}

impl EnforcementState {
    pub(crate) fn from_json(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub(crate) fn observe_weekly(&mut self, used_percent: f64, provider: &str) -> f64 {
        self.provider = Some(provider.to_string());
        let baseline = *self
            .weekly_baseline_percent
            .get_or_insert(used_percent.max(0.0));
        // A rolling quota can decrease as older usage leaves the provider's
        // window. Preserve already-consumed task budget in that case; only a
        // monotonic increase can add to this session's allowance.
        if used_percent >= baseline {
            self.weekly_consumed_percent =
                self.weekly_consumed_percent.max(used_percent - baseline);
        }
        self.weekly_consumed_percent
    }
}

pub(crate) fn validate_change(
    current: &TaskBudget,
    requested: &TaskBudget,
    has_started: bool,
) -> Result<(), String> {
    requested.validate()?;
    if !has_started {
        return Ok(());
    }
    if current == requested || requested.is_unlimited() {
        return Ok(());
    }
    match (current, requested) {
        (
            TaskBudget::Tokens {
                limit_tokens: before,
            },
            TaskBudget::Tokens {
                limit_tokens: after,
            },
        ) if after >= before => Ok(()),
        (
            TaskBudget::WeeklyPercent {
                limit_percent: before,
            },
            TaskBudget::WeeklyPercent {
                limit_percent: after,
            },
        ) if after >= before => Ok(()),
        _ => Err(
            "After the first turn, a task budget can only be increased or turned off while stopped."
                .into(),
        ),
    }
}

/// Reconciles provider usage notifications that may be cumulative, per-step,
/// or repeated. A decreasing report starts a new per-step sequence; an equal
/// report is treated as a duplicate.
#[derive(Debug, Default)]
pub(crate) struct UsageReconciler {
    last_input: u64,
    last_output: u64,
}

impl UsageReconciler {
    pub(crate) fn delta(&mut self, input: u64, output: u64) -> (u64, u64) {
        let delta = if input >= self.last_input && output >= self.last_output {
            (input - self.last_input, output - self.last_output)
        } else {
            (input, output)
        };
        self.last_input = input;
        self.last_output = output;
        delta
    }
}

pub(crate) fn token_reserve(limit: u64) -> u64 {
    (limit / 20).clamp(4_000, 20_000)
}

pub(crate) fn closeout_instruction() -> String {
    "Budget closeout: stop starting substantive work. Safely finish the operation already in flight, run only the highest-value validation that fits, then return a concise summary of completed work, remaining work, blockers, and current workspace state.".into()
}

pub(crate) fn progress_instruction() -> String {
    "Budget reminder: you are around halfway through the session target. Prioritize the highest-value work and reserve enough capacity for validation and a concise completed/remaining/current-state response.".into()
}

pub(crate) fn launch_instruction(budget: &TaskBudget) -> Option<String> {
    match budget {
        TaskBudget::Unlimited => None,
        TaskBudget::Tokens { limit_tokens } => Some(format!(
            "Session task budget: approximately {limit_tokens} total tokens across this session, including follow-up turns and provider changes. Prioritize the highest-value work and reserve capacity for validation plus a concise completed/remaining/current-state response. This is a graceful response-boundary target, so one response may overshoot it."
        )),
        TaskBudget::WeeklyPercent { limit_percent } => Some(format!(
            "Session task budget: allow this session to increase the account's 7-day usage by approximately {limit_percent} percentage points across all turns and provider changes. Prioritize the highest-value work and reserve capacity for validation plus a concise completed/remaining/current-state response. This is a graceful response-boundary target, so provider accounting may vary by one response."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_cumulative_and_duplicate_reports() {
        let mut usage = UsageReconciler::default();
        assert_eq!(usage.delta(100, 20), (100, 20));
        assert_eq!(usage.delta(100, 20), (0, 0));
        assert_eq!(usage.delta(150, 30), (50, 10));
        assert_eq!(usage.delta(20, 5), (20, 5));
    }

    #[test]
    fn reserves_are_clamped_for_graceful_closeout() {
        assert_eq!(token_reserve(10_000), 4_000);
        assert_eq!(token_reserve(1_000_000), 20_000);
        assert_eq!(token_reserve(100_000), 5_000);
    }

    #[test]
    fn started_sessions_can_only_top_up_or_turn_budget_off() {
        let current = TaskBudget::Tokens {
            limit_tokens: 50_000,
        };
        assert!(validate_change(
            &current,
            &TaskBudget::Tokens {
                limit_tokens: 100_000
            },
            true
        )
        .is_ok());
        assert!(validate_change(
            &current,
            &TaskBudget::Tokens {
                limit_tokens: 25_000
            },
            true
        )
        .is_err());
        assert!(validate_change(&current, &TaskBudget::Unlimited, true).is_ok());
    }

    #[test]
    fn weekly_usage_is_cumulative_and_fail_safe_on_decrease() {
        let mut state = EnforcementState::default();
        assert_eq!(state.observe_weekly(40.0, "codex"), 0.0);
        assert_eq!(state.observe_weekly(43.5, "codex"), 3.5);
        assert_eq!(state.observe_weekly(41.0, "codex"), 3.5);
        assert_eq!(state.observe_weekly(45.0, "codex"), 5.0);
    }
}
