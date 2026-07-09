use std::time::{Duration, Instant};

use am_proto::{RunCapacityPolicy, SystemCapacitySnapshot};
use serde_json::json;
use sysinfo::System;

use crate::{AppCore, CoreError};

const RUN_CAPACITY_POLICY_KEY: &str = "run_capacity_policy";
const DEFAULT_HARD_RUNTIME_CAP: usize = 512;
const IO_BOUND_SESSION_CPU_MULTIPLIER: usize = 4;
/// Reading memory via `sysinfo` allocates and syscalls; session starts and UI
/// polls hit this path, so serve a short-lived cached reading instead.
const SYSTEM_CAPACITY_CACHE_TTL: Duration = Duration::from_secs(5);

impl AppCore {
    pub async fn get_run_capacity_policy(&self) -> Result<RunCapacityPolicy, CoreError> {
        let raw = am_db::repos::settings::get(&self.db.pool, RUN_CAPACITY_POLICY_KEY).await?;
        let policy = raw
            .and_then(|value| serde_json::from_str::<RunCapacityPolicy>(&value).ok())
            .unwrap_or_default();
        Ok(normalize_capacity_policy(policy))
    }

    pub async fn set_run_capacity_policy(
        &self,
        policy: RunCapacityPolicy,
    ) -> Result<RunCapacityPolicy, CoreError> {
        let policy = normalize_capacity_policy(policy);
        let raw = serde_json::to_string(&policy).unwrap_or_default();
        am_db::repos::settings::set(&self.db.pool, RUN_CAPACITY_POLICY_KEY, &raw).await?;
        // Apply the new cap immediately: growing admits queued work now rather
        // than on the next session start.
        self.sync_session_capacity().await;
        self.activity(
            None,
            None,
            "capacity_policy.updated",
            json!({
                "adaptive": policy.adaptive,
                "manual_max_active_sessions": policy.manual_max_active_sessions,
                "hard_max_active_sessions": policy.hard_max_active_sessions,
                "allow_over_recommended": policy.allow_over_recommended,
            }),
        )
        .await?;
        Ok(policy)
    }

    pub async fn system_capacity_snapshot(&self) -> Result<SystemCapacitySnapshot, CoreError> {
        let policy = self.get_run_capacity_policy().await.unwrap_or_default();
        let system = self.read_system_capacity();
        let recommended = recommended_active_sessions(&policy, &system);
        let effective = effective_active_sessions(&policy, recommended);
        self.sessions.resize(effective);
        let active_sessions = self.sessions.active_count().await;
        let active_sandboxes = self.sandboxes.active_count().await;
        let warning = capacity_warning(&policy, recommended, effective);
        Ok(SystemCapacitySnapshot {
            logical_cpus: system.logical_cpus,
            total_memory_mb: system.total_memory_mb,
            available_memory_mb: system.available_memory_mb,
            recommended_active_sessions: recommended,
            effective_active_sessions: effective,
            active_sessions,
            queued_plan_nodes: self.sessions.queue_len(),
            active_sandboxes,
            warning,
        })
    }

    /// Refresh the resource-aware effective cap and apply it to the session
    /// admission controller. Returns the effective cap.
    pub(crate) async fn sync_session_capacity(&self) -> usize {
        let effective = self.effective_session_capacity().await;
        self.sessions.resize(effective);
        effective
    }

    pub(crate) async fn effective_session_capacity(&self) -> usize {
        let policy = self.get_run_capacity_policy().await.unwrap_or_default();
        let system = self.read_system_capacity();
        let recommended = recommended_active_sessions(&policy, &system);
        effective_active_sessions(&policy, recommended).clamp(1, DEFAULT_HARD_RUNTIME_CAP)
    }

    /// Soft capacity check for callers that run agent processes outside the
    /// session admission controller (e.g. gate evaluators).
    pub(crate) async fn ensure_session_capacity(&self) -> Result<(), CoreError> {
        let effective = self.sync_session_capacity().await;
        let active = self.sessions.active_count().await;
        if active >= effective {
            let policy = self.get_run_capacity_policy().await.unwrap_or_default();
            let system = self.read_system_capacity();
            let recommended = recommended_active_sessions(&policy, &system);
            return Err(CoreError::Other(capacity_limit_message(
                active,
                effective,
                recommended,
                &system,
                &policy,
            )));
        }
        Ok(())
    }

    pub(crate) async fn session_capacity_error(&self) -> CoreError {
        let policy = self.get_run_capacity_policy().await.unwrap_or_default();
        let system = self.read_system_capacity();
        let recommended = recommended_active_sessions(&policy, &system);
        let effective =
            effective_active_sessions(&policy, recommended).clamp(1, DEFAULT_HARD_RUNTIME_CAP);
        let active = self.sessions.active_count().await;
        CoreError::Other(capacity_limit_message(
            active,
            effective,
            recommended,
            &system,
            &policy,
        ))
    }

    fn read_system_capacity(&self) -> LocalSystemCapacity {
        {
            let cache = self.capacity_cache.lock().unwrap();
            if let Some((at, capacity)) = cache.as_ref() {
                if at.elapsed() < SYSTEM_CAPACITY_CACHE_TTL {
                    return capacity.clone();
                }
            }
        }
        let fresh = read_system_capacity();
        *self.capacity_cache.lock().unwrap() = Some((Instant::now(), fresh.clone()));
        fresh
    }
}

#[derive(Clone)]
pub(crate) struct LocalSystemCapacity {
    logical_cpus: usize,
    total_memory_mb: u64,
    available_memory_mb: u64,
}

fn read_system_capacity() -> LocalSystemCapacity {
    let mut system = System::new();
    system.refresh_memory();
    let logical_cpus = std::thread::available_parallelism()
        .map(|cpus| cpus.get())
        .unwrap_or(4);
    LocalSystemCapacity {
        logical_cpus,
        total_memory_mb: system.total_memory() / 1024 / 1024,
        available_memory_mb: system.available_memory() / 1024 / 1024,
    }
}

fn recommended_active_sessions(policy: &RunCapacityPolicy, system: &LocalSystemCapacity) -> usize {
    let spare_cpus = system
        .logical_cpus
        .saturating_sub(policy.reserved_cpus)
        .max(1);
    let cpu_based = spare_cpus.saturating_mul(IO_BOUND_SESSION_CPU_MULTIPLIER);
    // `available_memory` can dip sharply under macOS memory pressure and cache
    // churn even when the machine can comfortably queue more IO-bound CLI
    // sessions. Keep the recommendation resource-aware, but don't let a
    // transient low reading collapse a capable machine to one session.
    let usable_memory_mb = system
        .available_memory_mb
        .max(system.total_memory_mb / 4)
        .max(policy.memory_per_session_mb.max(256));
    let memory_based = (usable_memory_mb / policy.memory_per_session_mb.max(256)).max(1);
    cpu_based
        .min(memory_based as usize)
        .clamp(1, policy.hard_max_active_sessions.max(1))
}

fn effective_active_sessions(policy: &RunCapacityPolicy, recommended: usize) -> usize {
    let requested = if policy.adaptive {
        recommended
    } else {
        policy.manual_max_active_sessions.unwrap_or(recommended)
    };
    if policy.allow_over_recommended {
        requested
    } else {
        requested.min(recommended)
    }
    .clamp(1, policy.hard_max_active_sessions.max(1))
}

fn capacity_warning(
    policy: &RunCapacityPolicy,
    recommended: usize,
    effective: usize,
) -> Option<String> {
    if effective > recommended {
        Some(format!(
            "Active cap is above the current resource-aware recommendation ({recommended})."
        ))
    } else if policy.hard_max_active_sessions > DEFAULT_HARD_RUNTIME_CAP {
        Some("Hard cap is unusually high; monitor CPU, memory, and provider limits.".into())
    } else {
        None
    }
}

fn normalize_capacity_policy(mut policy: RunCapacityPolicy) -> RunCapacityPolicy {
    policy.reserved_cpus = policy.reserved_cpus.min(64);
    policy.memory_per_session_mb = policy.memory_per_session_mb.clamp(256, 131_072);
    policy.hard_max_active_sessions = policy
        .hard_max_active_sessions
        .clamp(1, DEFAULT_HARD_RUNTIME_CAP);
    if let Some(max) = policy.manual_max_active_sessions {
        policy.manual_max_active_sessions = Some(max.clamp(1, policy.hard_max_active_sessions));
    }
    policy
}

fn capacity_limit_message(
    active: usize,
    effective: usize,
    recommended: usize,
    system: &LocalSystemCapacity,
    policy: &RunCapacityPolicy,
) -> String {
    let mode = if policy.adaptive {
        "adaptive"
    } else {
        "manual"
    };
    format!(
        "Active agent capacity reached ({active}/{effective}). The {mode} cap is based on {cpus} CPU threads, {memory_mb} MB available memory, and a {per_session_mb} MB/session budget; current recommendation is {recommended}. Wait for an active agent to finish, stop one, or raise Run Capacity if this machine can handle it.",
        cpus = system.logical_cpus,
        memory_mb = system.available_memory_mb,
        per_session_mb = policy.memory_per_session_mb.max(256),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_recommendation_uses_cpu_and_memory() {
        let policy = RunCapacityPolicy::default();
        let system = LocalSystemCapacity {
            logical_cpus: 16,
            total_memory_mb: 32_768,
            available_memory_mb: 8_192,
        };
        assert_eq!(recommended_active_sessions(&policy, &system), 16);
    }

    #[test]
    fn capacity_recommendation_does_not_collapse_on_transient_low_available_memory() {
        let policy = RunCapacityPolicy::default();
        let system = LocalSystemCapacity {
            logical_cpus: 12,
            total_memory_mb: 32_768,
            available_memory_mb: 384,
        };
        assert_eq!(recommended_active_sessions(&policy, &system), 16);
    }

    #[test]
    fn manual_override_requires_over_recommended_opt_in() {
        let mut policy = RunCapacityPolicy {
            adaptive: false,
            manual_max_active_sessions: Some(40),
            allow_over_recommended: false,
            ..Default::default()
        };
        assert_eq!(effective_active_sessions(&policy, 8), 8);
        policy.allow_over_recommended = true;
        assert_eq!(effective_active_sessions(&policy, 8), 40);
    }

    #[test]
    fn hard_cap_clamps_to_runtime_maximum() {
        let policy = normalize_capacity_policy(RunCapacityPolicy {
            hard_max_active_sessions: 10_000,
            ..Default::default()
        });
        assert_eq!(policy.hard_max_active_sessions, DEFAULT_HARD_RUNTIME_CAP);
    }

    #[test]
    fn capacity_message_explains_resource_limit() {
        let policy = RunCapacityPolicy::default();
        let system = LocalSystemCapacity {
            logical_cpus: 8,
            total_memory_mb: 16_384,
            available_memory_mb: 2_048,
        };
        let message = capacity_limit_message(4, 4, 4, &system, &policy);
        assert!(message.contains("Active agent capacity reached (4/4)"));
        assert!(message.contains("8 CPU threads"));
        assert!(message.contains("2048 MB available memory"));
    }
}
