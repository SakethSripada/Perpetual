use am_proto::{AgentKind, AgentStatus, AppEvent, AvailabilityState, TaskStatus, TaskUpdate};
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::{AppCore, CoreError};

#[derive(Debug, Clone)]
pub(crate) struct FallbackPolicy {
    pub auto_switch: bool,
    pub switch_back: bool,
    pub preferred_order: Vec<AgentKind>,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            auto_switch: true,
            switch_back: true,
            preferred_order: vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Cursor],
        }
    }
}

impl From<&am_proto::LimitPolicy> for FallbackPolicy {
    fn from(policy: &am_proto::LimitPolicy) -> Self {
        Self {
            auto_switch: policy.auto_switch,
            switch_back: policy.switch_back,
            preferred_order: policy.agent_priority.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FallbackDecision {
    Switch { agent: AgentKind, switch_back: bool },
    Wait { reset_at: Option<DateTime<Utc>> },
    Disabled,
}

impl FallbackPolicy {
    pub(crate) fn decide(
        &self,
        current: AgentKind,
        statuses: &[AgentStatus],
        reset_at: Option<DateTime<Utc>>,
    ) -> FallbackDecision {
        if !self.auto_switch {
            return FallbackDecision::Disabled;
        }

        for preferred in &self.preferred_order {
            if *preferred == current {
                continue;
            }
            if statuses
                .iter()
                .any(|status| status.kind == *preferred && agent_ready(status))
            {
                return FallbackDecision::Switch {
                    agent: *preferred,
                    switch_back: self.switch_back,
                };
            }
        }

        for status in statuses {
            if status.kind != current && agent_ready(status) {
                return FallbackDecision::Switch {
                    agent: status.kind,
                    switch_back: self.switch_back,
                };
            }
        }

        FallbackDecision::Wait { reset_at }
    }
}

impl AppCore {
    pub(crate) async fn fallback_decision(
        &self,
        current: AgentKind,
        reset_at: Option<DateTime<Utc>>,
    ) -> Result<FallbackDecision, CoreError> {
        let statuses = self.detect_agents().await?;
        let policy = self.get_limit_policy().await.unwrap_or_default();
        Ok(FallbackPolicy::from(&policy).decide(current, &statuses, reset_at))
    }

    pub(crate) async fn apply_fallback_decision(
        &self,
        task_id: &str,
        project_id: &str,
        current: AgentKind,
        reset_at: Option<DateTime<Utc>>,
    ) -> Result<FallbackDecision, CoreError> {
        let decision = self.fallback_decision(current, reset_at).await?;
        match decision.clone() {
            FallbackDecision::Switch { agent, switch_back } => {
                if let Ok(task) = am_db::repos::task::update(
                    &self.db.pool,
                    task_id,
                    TaskUpdate {
                        status: Some(TaskStatus::Queued),
                        primary_agent: Some(agent),
                        ..Default::default()
                    },
                )
                .await
                {
                    self.events.publish(AppEvent::TaskUpdated(task));
                }
                self.activity(
                    Some(project_id.to_string()),
                    Some(task_id.to_string()),
                    "fallback.switch_queued",
                    json!({
                        "from": current.as_str(),
                        "to": agent.as_str(),
                        "switch_back": switch_back,
                    }),
                )
                .await?;
            }
            FallbackDecision::Wait { reset_at } => {
                self.activity(
                    Some(project_id.to_string()),
                    Some(task_id.to_string()),
                    "fallback.waiting",
                    json!({
                        "agent": current.as_str(),
                        "reset_at": reset_at,
                    }),
                )
                .await?;
            }
            FallbackDecision::Disabled => {
                self.activity(
                    Some(project_id.to_string()),
                    Some(task_id.to_string()),
                    "fallback.disabled",
                    json!({ "agent": current.as_str() }),
                )
                .await?;
            }
        }

        Ok(decision)
    }
}

fn agent_ready(status: &AgentStatus) -> bool {
    status.installed && status.authenticated && status.availability != AvailabilityState::Limited
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(kind: AgentKind, ready: bool) -> AgentStatus {
        AgentStatus {
            kind,
            installed: ready,
            authenticated: ready,
            version: None,
            binary_path: None,
            availability: if ready {
                AvailabilityState::Available
            } else {
                AvailabilityState::Unknown
            },
            reset_at: None,
            last_checked: None,
        }
    }

    #[test]
    fn switches_to_next_preferred_ready_agent() {
        let policy = FallbackPolicy::default();
        let decision = policy.decide(
            AgentKind::ClaudeCode,
            &[
                status(AgentKind::ClaudeCode, false),
                status(AgentKind::Codex, true),
            ],
            None,
        );

        assert_eq!(
            decision,
            FallbackDecision::Switch {
                agent: AgentKind::Codex,
                switch_back: true
            }
        );
    }

    #[test]
    fn waits_when_no_fallback_agent_is_ready() {
        let policy = FallbackPolicy::default();
        let reset_at = Some(Utc::now());
        let decision = policy.decide(
            AgentKind::ClaudeCode,
            &[status(AgentKind::Codex, false)],
            reset_at,
        );

        assert_eq!(decision, FallbackDecision::Wait { reset_at });
    }

    #[test]
    fn disabled_policy_does_not_switch() {
        let policy = FallbackPolicy {
            auto_switch: false,
            ..FallbackPolicy::default()
        };

        assert_eq!(
            policy.decide(
                AgentKind::ClaudeCode,
                &[status(AgentKind::Codex, true)],
                None,
            ),
            FallbackDecision::Disabled
        );
    }
}
