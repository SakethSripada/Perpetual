use am_agents::{AgentInstallStatus, QuotaWindowKind};
use am_proto::{
    now, AgentKind, AgentStatus, AvailabilityState, ProviderUsage, ProviderUsageWindow,
};
use chrono::{DateTime, Utc};

use crate::{AppCore, CoreError};

const READY_AGENT_CACHE_SECS: i64 = 60;
/// Ceiling for the exponential unknown-reset probe backoff.
const MAX_UNKNOWN_RESET_BACKOFF_SECS: i64 = 15 * 60;

impl AppCore {
    pub(crate) fn provider_usage(&self, kind: AgentKind) -> Option<ProviderUsage> {
        self.provider_usage
            .lock()
            .ok()
            .and_then(|usage| usage.get(&kind).cloned())
    }

    pub(crate) fn update_provider_usage(
        &self,
        kind: AgentKind,
        window: QuotaWindowKind,
        used_percent: f64,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> ProviderUsage {
        let mut all_usage = self
            .provider_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let usage = all_usage.entry(kind).or_default();
        let sample = Some(ProviderUsageWindow {
            used_percent: used_percent.clamp(0.0, 100.0),
            reset_at,
        });
        match window {
            QuotaWindowKind::FiveHour => usage.five_hour = sample,
            QuotaWindowKind::Weekly => usage.weekly = sample,
        }
        usage.clone()
    }

    pub(crate) async fn fresh_ready_agent_status(
        &self,
        kind: AgentKind,
    ) -> Result<Option<AgentStatus>, CoreError> {
        let Some(record) = am_db::repos::agent::get(&self.db.pool, kind).await? else {
            return Ok(None);
        };
        if record.install_status != "installed"
            || record.availability != AvailabilityState::Available
        {
            return Ok(None);
        }
        let Some(last_checked) = record.last_checked else {
            return Ok(None);
        };
        if now().signed_duration_since(last_checked)
            > chrono::Duration::seconds(READY_AGENT_CACHE_SECS)
        {
            return Ok(None);
        }

        Ok(Some(AgentStatus {
            kind,
            installed: true,
            authenticated: true,
            version: record.version,
            binary_path: None,
            availability: record.availability,
            reset_at: record.reset_at,
            last_checked: record.last_checked,
            usage: self.provider_usage(kind),
        }))
    }

    pub(crate) async fn record_agent_probe(
        &self,
        detected: AgentInstallStatus,
    ) -> Result<AgentStatus, CoreError> {
        let existing = am_db::repos::agent::get(&self.db.pool, detected.kind).await?;
        let ready = detected.installed && detected.authenticated;
        let ts = now();

        let preserve_limited = existing.as_ref().is_some_and(|record| {
            record.availability == AvailabilityState::Limited
                && record.reset_at.is_some_and(|reset_at| reset_at > ts)
        });

        let availability = if preserve_limited {
            AvailabilityState::Limited
        } else if ready {
            AvailabilityState::Available
        } else {
            AvailabilityState::Unknown
        };
        let reset_at = if preserve_limited {
            existing.as_ref().and_then(|record| record.reset_at)
        } else {
            None
        };

        let record = am_db::repos::agent::AgentRecord {
            kind: detected.kind,
            install_status: install_status(detected.installed, detected.authenticated).to_string(),
            version: detected.version.clone(),
            availability,
            reset_at,
            last_checked: Some(ts),
            // Strikes persist while a limit is preserved; a healthy probe that
            // clears the limit also clears the backoff.
            limit_strikes: if preserve_limited {
                existing
                    .as_ref()
                    .map(|record| record.limit_strikes)
                    .unwrap_or(0)
            } else {
                0
            },
        };
        let saved = am_db::repos::agent::upsert(&self.db.pool, &record).await?;

        Ok(AgentStatus {
            kind: detected.kind,
            installed: detected.installed,
            authenticated: detected.authenticated,
            version: detected.version,
            binary_path: detected
                .binary_path
                .map(|p| p.to_string_lossy().to_string()),
            availability: saved.availability,
            reset_at: saved.reset_at,
            last_checked: saved.last_checked,
            usage: self.provider_usage(detected.kind),
        })
    }

    pub(crate) async fn mark_agent_limited(
        &self,
        kind: AgentKind,
        reset_at: Option<DateTime<Utc>>,
    ) -> Result<(), CoreError> {
        // When the provider doesn't tell us when the limit resets, synthesize a
        // bounded retry time so the agent is re-probed instead of being
        // stranded as "limited forever". Consecutive unknown-reset limits back
        // off exponentially (base, 2x, 4x... capped at 15 min) so a persistent
        // limit isn't probed on a tight loop; a provider-supplied reset time
        // or a successful recovery clears the strikes.
        let (reset_at, strikes) = match reset_at {
            Some(reset_at) => (Some(reset_at), 0),
            None => {
                let base = self
                    .get_limit_policy()
                    .await
                    .unwrap_or_default()
                    .unknown_reset_retry_secs;
                let prior = am_db::repos::agent::get(&self.db.pool, kind)
                    .await?
                    .map(|record| record.limit_strikes)
                    .unwrap_or(0);
                let strikes = prior + 1;
                let backoff = (base.max(1) as i64)
                    .saturating_mul(1_i64 << (strikes - 1).clamp(0, 16))
                    .min(MAX_UNKNOWN_RESET_BACKOFF_SECS);
                (
                    (base > 0).then(|| now() + chrono::Duration::seconds(backoff)),
                    strikes,
                )
            }
        };
        am_db::repos::agent::mark_limited(&self.db.pool, kind, reset_at, strikes).await?;
        // Wake the scheduler exactly when the limit lifts instead of within
        // ±30s of it.
        if let Some(reset_at) = reset_at {
            self.note_limit_reset_deadline(reset_at);
        }
        Ok(())
    }

    pub(crate) async fn mark_agent_available(&self, kind: AgentKind) -> Result<(), CoreError> {
        am_db::repos::agent::mark_available(&self.db.pool, kind).await?;
        // Limit-waiting work may be resumable immediately.
        self.wake_scheduler();
        Ok(())
    }
}

fn install_status(installed: bool, authenticated: bool) -> &'static str {
    if !installed {
        "not_installed"
    } else if !authenticated {
        "unauthenticated"
    } else {
        "installed"
    }
}

#[cfg(test)]
mod tests {
    use super::install_status;

    #[test]
    fn install_status_maps_install_and_auth() {
        assert_eq!(install_status(false, false), "not_installed");
        assert_eq!(install_status(false, true), "not_installed");
        assert_eq!(install_status(true, false), "unauthenticated");
        assert_eq!(install_status(true, true), "installed");
    }
}
