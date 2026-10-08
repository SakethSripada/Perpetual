use am_agents::QuotaWindowKind;
use am_proto::TaskBudget;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct EnforcementState {
    pub(crate) five_hour_baseline_percent: Option<f64>,
    pub(crate) five_hour_consumed_percent: f64,
    pub(crate) weekly_baseline_percent: Option<f64>,
    pub(crate) weekly_consumed_percent: f64,
    pub(crate) weekly_offset_percent: f64,
    pub(crate) weekly_reset_at: Option<DateTime<Utc>>,
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

    pub(crate) fn observe(
        &mut self,
        window: QuotaWindowKind,
        used_percent: f64,
        provider: &str,
        reset_at: Option<DateTime<Utc>>,
    ) -> f64 {
        let provider_changed = self.provider.as_deref().is_some_and(|old| old != provider);
        self.provider = Some(provider.to_string());
        let reset_changed = window == QuotaWindowKind::Weekly
            && reset_at.is_some_and(|next| self.weekly_reset_at.is_some_and(|old| old != next));
        if window == QuotaWindowKind::Weekly {
            if let Some(reset_at) = reset_at {
                self.weekly_reset_at = Some(reset_at);
            }
            if provider_changed || reset_changed {
                self.weekly_offset_percent = self.weekly_consumed_percent;
                self.weekly_baseline_percent = None;
            }
        }
        let (baseline, consumed) = match window {
            QuotaWindowKind::FiveHour => (
                &mut self.five_hour_baseline_percent,
                &mut self.five_hour_consumed_percent,
            ),
            QuotaWindowKind::Weekly => (
                &mut self.weekly_baseline_percent,
                &mut self.weekly_consumed_percent,
            ),
        };
        let baseline = *baseline.get_or_insert(used_percent.max(0.0));
        // A rolling quota can decrease as older usage leaves the provider's
        // window. Preserve already-consumed task budget in that case; only a
        // monotonic increase can add to this session's allowance.
        if used_percent >= baseline {
            let offset = if window == QuotaWindowKind::Weekly {
                self.weekly_offset_percent
            } else {
                0.0
            };
            *consumed = consumed.max(offset + used_percent - baseline);
        }
        *consumed
    }
}

pub(crate) fn quota_limit(budget: &TaskBudget, window: QuotaWindowKind) -> Option<f64> {
    match (budget, window) {
        (TaskBudget::WeeklyPercent { limit_percent }, QuotaWindowKind::Weekly) => {
            Some(f64::from(*limit_percent))
        }
        _ => None,
    }
}

pub(crate) fn is_percentage_budget(budget: &TaskBudget) -> bool {
    matches!(budget, TaskBudget::WeeklyPercent { .. })
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

pub(crate) fn token_reserve(limit: u64) -> u64 {
    (limit / 20).clamp(4_000, 20_000)
}

pub(crate) fn closeout_instruction() -> String {
    "Budget closeout: stop starting substantive work. Safely finish the operation already in flight, run only the highest-value validation that fits, then return a concise summary of completed work, remaining work, blockers, and current workspace state.".into()
}

pub(crate) fn progress_instruction() -> String {
    "Budget reminder: you are around halfway through the session target. Prioritize the highest-value work and reserve enough capacity for validation and a concise completed/remaining/current-state response.".into()
}

pub(crate) fn launch_instruction(
    budget: &TaskBudget,
    used_tokens: u64,
    used_weekly_percent: f64,
) -> Option<String> {
    match budget {
        TaskBudget::Unlimited => None,
        TaskBudget::Tokens { limit_tokens } => {
            let remaining = limit_tokens.saturating_sub(used_tokens);
            Some(format!(
                "Session task budget: {limit_tokens} total input and output tokens across all turns and provider changes. Provider reports show {used_tokens} used before this turn, leaving about {remaining} tokens. Plan this turn within the remaining allowance; reserve capacity for validation and a concise completed/remaining/current-state response. Usage arrives after model steps, so one response may overshoot the cap."
            ))
        }
        TaskBudget::WeeklyPercent { limit_percent } => {
            let remaining = (f64::from(*limit_percent) - used_weekly_percent).max(0.0);
            Some(format!(
                "Session task budget: increase the selected Codex account's 7-day usage by at most {limit_percent} percentage points across all turns. Provider reports show approximately {used_weekly_percent:.1} points consumed before this turn, leaving about {remaining:.1} points. Plan this turn within the remaining allowance; reserve capacity for validation and a concise completed/remaining/current-state response. Account usage is reported after responses, so one response may overshoot the target."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 40.0, "codex", None),
            0.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 43.5, "codex", None),
            3.5
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 41.0, "codex", None),
            3.5
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 45.0, "codex", None),
            5.0
        );
    }

    #[test]
    fn weekly_usage_accumulates_across_account_and_window_changes() {
        let mut state = EnforcementState::default();
        let first = DateTime::parse_from_rfc3339("2026-10-05T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let second = DateTime::parse_from_rfc3339("2026-10-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 40.0, "account-a", Some(first)),
            0.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 43.0, "account-a", Some(first)),
            3.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 10.0, "account-b", Some(first)),
            3.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 12.0, "account-b", Some(first)),
            5.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 1.0, "account-b", Some(second)),
            5.0
        );
        assert_eq!(
            state.observe(QuotaWindowKind::Weekly, 2.5, "account-b", Some(second)),
            6.5
        );
    }

    #[test]
    fn launch_guidance_reports_remaining_allowance() {
        let tokens = launch_instruction(
            &TaskBudget::Tokens {
                limit_tokens: 50_000,
            },
            12_500,
            0.0,
        )
        .unwrap();
        assert!(tokens.contains("leaving about 37500 tokens"));
        let weekly =
            launch_instruction(&TaskBudget::WeeklyPercent { limit_percent: 5 }, 0, 1.5).unwrap();
        assert!(weekly.contains("leaving about 3.5 points"));
    }
}
