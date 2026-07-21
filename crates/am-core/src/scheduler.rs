use am_agents::PermissionPolicy;
use am_db::repos::agent::AgentRecord;
use am_proto::{now, AgentKind, AppEvent, AvailabilityState, Task, TaskStatus, TaskUpdate};
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};

use crate::agent_thread::parse_permission;
use crate::{AppCore, CoreError};

/// Safety-net tick: the scheduler is normally woken by state changes (session
/// end, work queued, limit marked) and by the exact next limit-reset deadline;
/// this interval only bounds how stale things can get if a wake signal is
/// missed.
const SCHEDULER_INTERVAL: Duration = Duration::from_secs(30);
/// Per-tick start budget. Ticks are capacity-guarded and wake-driven, so a
/// larger batch drains queues fast without racing retryable start errors.
const SCHEDULER_BATCH_LIMIT: i64 = 32;
/// Coalesce bursts of wake signals (a plan finishing ends many sessions at
/// once) into a single tick.
const SCHEDULER_WAKE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Event-driven background runner for queued continuations and limit resets.
pub(crate) struct Scheduler {
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self {
            handle: Mutex::new(None),
        }
    }

    pub(crate) async fn start(&self, core: AppCore) {
        let mut handle = self.handle.lock().await;
        if handle.is_some() {
            return;
        }

        *handle = Some(tokio::spawn(async move {
            loop {
                let sleep_for = core.next_scheduler_delay();
                tokio::select! {
                    _ = time::sleep(sleep_for) => {}
                    _ = core.scheduler_wake.notified() => {
                        time::sleep(SCHEDULER_WAKE_DEBOUNCE).await;
                    }
                }
                core.run_scheduler_tick().await;
            }
        }));
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(handle) = self.handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl AppCore {
    /// Wake the scheduler loop now instead of waiting for the next tick. Call
    /// after any state change that could make queued/waiting work runnable.
    pub(crate) fn wake_scheduler(&self) {
        self.scheduler_wake.notify_one();
    }

    /// Record a provider-limit reset time so the scheduler wakes exactly then.
    /// Keeps the earliest pending deadline.
    pub(crate) fn note_limit_reset_deadline(&self, reset_at: DateTime<Utc>) {
        {
            let mut deadline = self.next_reset_deadline.lock().unwrap();
            match *deadline {
                Some(current) if current <= reset_at => {}
                _ => *deadline = Some(reset_at),
            }
        }
        // Recompute the loop's sleep with the new deadline in view.
        self.wake_scheduler();
    }

    /// How long the scheduler should sleep absent a wake signal: until the
    /// next limit reset if that's sooner than the safety-net interval.
    fn next_scheduler_delay(&self) -> Duration {
        let mut deadline = self.next_reset_deadline.lock().unwrap();
        let Some(at) = *deadline else {
            return SCHEDULER_INTERVAL;
        };
        let until = at - now();
        match until.to_std() {
            // Add a small buffer so we tick just after the reset, not just before.
            Ok(delay) => delay
                .saturating_add(Duration::from_millis(500))
                .min(SCHEDULER_INTERVAL),
            Err(_) => {
                // Deadline already passed; consume it and tick immediately.
                *deadline = None;
                Duration::ZERO
            }
        }
    }
}

impl AppCore {
    pub(crate) async fn run_scheduler_tick(&self) {
        if let Err(err) = self.run_queued_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "queued", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_reset_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "limit_reset", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_network_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "network", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_thread_queued_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "thread_queued", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_thread_network_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "thread_network", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_thread_reset_continuations().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "thread_limit_reset", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_cloud_monitor_tick().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "cloud_monitor", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_cloud_checkpoints().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "cloud_checkpoint", "error": err.to_string() }),
                )
                .await;
        }

        if let Err(err) = self.run_resumable_work_plans().await {
            let _ = self
                .activity(
                    None,
                    None,
                    "scheduler.tick_failed",
                    json!({ "phase": "work_plans", "error": err.to_string() }),
                )
                .await;
        }
    }

    /// Whether the session pool can admit more work right now. Checked before
    /// batch starts so full-capacity ticks skip cleanly instead of burning
    /// each start attempt on a "maximum concurrent" error; the wake on permit
    /// release re-runs the tick the moment a slot frees.
    async fn scheduler_has_capacity(&self) -> bool {
        let effective = self.sync_session_capacity().await;
        self.sessions.active_count().await < effective
    }

    async fn run_queued_continuations(&self) -> Result<(), CoreError> {
        if !self.scheduler_has_capacity().await {
            return Ok(());
        }
        let tasks = am_db::repos::task::list_for_status(
            &self.db.pool,
            TaskStatus::Queued,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;

        for task in tasks {
            self.try_scheduler_start(task, "scheduler.queued_continue")
                .await;
        }

        Ok(())
    }

    async fn run_network_continuations(&self) -> Result<(), CoreError> {
        let policy = self.get_local_model_policy().await.unwrap_or_default();
        if !policy.auto_resume_cloud {
            return Ok(());
        }
        let tasks = am_db::repos::task::list_for_status(
            &self.db.pool,
            TaskStatus::WaitingForNetwork,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;
        if tasks.is_empty() || !self.cloud_connectivity_stable(&policy).await? {
            return Ok(());
        }
        for task in tasks {
            self.try_scheduler_start(task, "scheduler.network_restored_continue")
                .await;
        }
        Ok(())
    }

    async fn run_reset_continuations(&self) -> Result<(), CoreError> {
        if !self.scheduler_has_capacity().await {
            return Ok(());
        }
        let tasks = am_db::repos::task::list_for_status(
            &self.db.pool,
            TaskStatus::WaitingForLimit,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;

        for task in tasks {
            if self.limit_wait_ready(&task).await? {
                self.try_scheduler_start(task, "scheduler.limit_reset_continue")
                    .await;
            }
        }

        Ok(())
    }

    async fn limit_wait_ready(&self, task: &Task) -> Result<bool, CoreError> {
        let Some(agent) = task.primary_agent else {
            return Ok(false);
        };
        let record = am_db::repos::agent::get(&self.db.pool, agent).await?;
        Ok(agent_record_ready_for_retry(record.as_ref(), now()))
    }

    async fn try_scheduler_start(&self, task: Task, kind: &str) {
        let Some(agent) = task.primary_agent else {
            return;
        };
        if self.sessions.is_active(&task.id).await {
            return;
        }

        let _ = self
            .activity(
                Some(task.project_id.clone()),
                Some(task.id.clone()),
                kind,
                json!({ "agent": agent.as_str() }),
            )
            .await;

        match self
            .run_task(&task.id, agent, PermissionPolicy::WorkspaceWrite)
            .await
        {
            Ok(session_id) => {
                let _ = self
                    .activity(
                        Some(task.project_id),
                        Some(task.id),
                        "scheduler.started",
                        json!({ "agent": agent.as_str(), "session_id": session_id }),
                    )
                    .await;
            }
            Err(err) => {
                self.handle_scheduler_start_error(&task, agent, err).await;
            }
        }
    }

    async fn handle_scheduler_start_error(&self, task: &Task, agent: AgentKind, err: CoreError) {
        let error = err.to_string();
        let retryable = scheduler_start_error_is_retryable(&error);

        let _ = self
            .activity(
                Some(task.project_id.clone()),
                Some(task.id.clone()),
                "scheduler.start_failed",
                json!({
                    "agent": agent.as_str(),
                    "error": error,
                    "retryable": retryable,
                }),
            )
            .await;

        if retryable {
            return;
        }

        if let Ok(updated) = am_db::repos::task::update(
            &self.db.pool,
            &task.id,
            TaskUpdate {
                status: Some(TaskStatus::Paused),
                ..Default::default()
            },
        )
        .await
        {
            self.events.publish(AppEvent::TaskUpdated(updated));
        }
    }

    async fn run_thread_queued_continuations(&self) -> Result<(), CoreError> {
        if !self.scheduler_has_capacity().await {
            return Ok(());
        }
        let threads = am_db::repos::agent_thread::list_for_status(
            &self.db.pool,
            TaskStatus::Queued,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;

        for thread in threads {
            let Some(agent) = thread.active_agent.or(thread.preferred_agent) else {
                continue;
            };
            self.try_thread_scheduler_start(thread, agent, "scheduler.thread_queued_continue")
                .await;
        }

        Ok(())
    }

    async fn run_resumable_work_plans(&self) -> Result<(), CoreError> {
        // One targeted query for paused runs, then a point lookup per gate
        // node — no per-project full-graph loads.
        let plans = am_db::repos::work_graph::list_paused_plan_runs(&self.db.pool).await?;
        for plan in plans {
            let Some(node_id) = plan.resume_after_node_id.as_deref() else {
                continue;
            };
            let waiting_node_done = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
                .await?
                .is_some_and(|node| node.status == TaskStatus::Done);
            if waiting_node_done {
                let _ = self.resume_work_plan(&plan.id).await;
            }
        }
        Ok(())
    }

    async fn run_thread_reset_continuations(&self) -> Result<(), CoreError> {
        if !self.scheduler_has_capacity().await {
            return Ok(());
        }
        let threads = am_db::repos::agent_thread::list_for_status(
            &self.db.pool,
            TaskStatus::WaitingForLimit,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;

        if threads.is_empty() {
            return Ok(());
        }
        // One live probe per tick refreshes agent availability (limited → ready
        // once a reset time passes) and is reused across the batch.
        let statuses = self.detect_agents().await?;
        let policy = self.get_limit_policy().await.unwrap_or_default();

        for thread in threads {
            if let Some(agent) = thread_resume_agent(&thread, &policy, &statuses) {
                self.try_thread_scheduler_start(
                    thread,
                    agent,
                    "scheduler.thread_limit_reset_continue",
                )
                .await;
            }
        }

        Ok(())
    }

    async fn run_thread_network_continuations(&self) -> Result<(), CoreError> {
        let policy = self.get_local_model_policy().await.unwrap_or_default();
        let threads = am_db::repos::agent_thread::list_for_status(
            &self.db.pool,
            TaskStatus::WaitingForNetwork,
            SCHEDULER_BATCH_LIMIT,
        )
        .await?;
        if threads.is_empty() {
            return Ok(());
        }

        let network_stable = if policy.auto_resume_cloud || policy.switch_back_to_cloud {
            self.cloud_connectivity_stable(&policy).await?
        } else {
            false
        };

        for thread in threads {
            if self.sessions.is_active(&thread.id).await {
                continue;
            }
            if network_stable {
                let agent = thread
                    .original_agent
                    .or(thread.active_agent)
                    .or(thread.preferred_agent);
                if let Some(agent) = agent {
                    self.resume_thread_cloud_after_network(
                        &thread.id,
                        agent,
                        "scheduler.thread_network_restored_continue",
                    )
                    .await;
                }
                continue;
            }

            if policy.use_local_fallback && thread.local_provider.is_none() {
                if let Some(agent) = thread.active_agent.or(thread.preferred_agent) {
                    if let Ok(Some(target)) = self.best_ready_local_target(&policy).await {
                        self.start_thread_local_fallback(&thread.id, agent, target, &policy)
                            .await;
                    }
                }
            }
        }

        Ok(())
    }

    async fn try_thread_scheduler_start(
        &self,
        thread: am_proto::AgentThread,
        agent: AgentKind,
        kind: &str,
    ) {
        if self.sessions.is_active(&thread.id).await {
            return;
        }
        // Resuming with a different agent than was active: record the switch so
        // the inspector/handoff state and switch-back bookkeeping stay correct.
        if thread.active_agent != Some(agent) {
            if let Ok(Some(mut latest)) =
                am_db::repos::agent_thread::get(&self.db.pool, &thread.id).await
            {
                if latest.original_agent.is_none() {
                    latest.original_agent = latest.active_agent;
                }
                latest.fallback_agent = Some(agent);
                latest.active_agent = Some(agent);
                if latest.execution_backend == am_proto::ExecutionBackend::DockerSandbox
                    && agent != AgentKind::Codex
                {
                    latest.execution_backend = am_proto::ExecutionBackend::Host;
                }
                latest.handoff_state = "fallback_active".to_string();
                if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &latest).await {
                    self.events.publish(AppEvent::AgentThreadUpdated(saved));
                }
            }
        }

        let _ = self
            .activity(
                thread.project_id.clone(),
                None,
                kind,
                json!({ "thread_id": thread.id, "agent": agent.as_str() }),
            )
            .await;

        let permission = parse_permission(&thread.permission);
        match self
            .run_agent_thread(&thread.id, agent, permission, None)
            .await
        {
            Ok(turn_id) => {
                let _ = self
                    .activity(
                        thread.project_id,
                        None,
                        "scheduler.thread_started",
                        json!({ "thread_id": thread.id, "agent": agent.as_str(), "turn_id": turn_id }),
                    )
                    .await;
            }
            Err(err) => {
                self.handle_thread_scheduler_start_error(&thread, agent, err)
                    .await;
            }
        }
    }

    async fn handle_thread_scheduler_start_error(
        &self,
        thread: &am_proto::AgentThread,
        agent: AgentKind,
        err: CoreError,
    ) {
        let error = err.to_string();
        let retryable = scheduler_start_error_is_retryable(&error);

        let _ = self
            .activity(
                thread.project_id.clone(),
                None,
                "scheduler.thread_start_failed",
                json!({
                    "thread_id": thread.id,
                    "agent": agent.as_str(),
                    "error": error,
                    "retryable": retryable,
                }),
            )
            .await;

        if retryable {
            return;
        }

        let _ = am_db::repos::agent_thread::update(
            &self.db.pool,
            &thread.id,
            am_proto::AgentThreadUpdate {
                status: Some(TaskStatus::Paused),
                ..Default::default()
            },
        )
        .await
        .map(|updated| self.events.publish(AppEvent::AgentThreadUpdated(updated)));
    }
}

/// Pick the agent to resume a limit-waiting thread with, or `None` to keep
/// waiting. With `resume_with_earliest` we take the first ready agent in
/// priority order (so whichever limit lifts first wins); otherwise we only
/// resume once the originally-active agent recovers.
fn thread_resume_agent(
    thread: &am_proto::AgentThread,
    policy: &am_proto::LimitPolicy,
    statuses: &[am_proto::AgentStatus],
) -> Option<AgentKind> {
    let ready = |agent: AgentKind| {
        statuses.iter().any(|status| {
            status.kind == agent
                && status.installed
                && status.authenticated
                && status.availability != AvailabilityState::Limited
        })
    };

    let active = thread.active_agent.or(thread.preferred_agent);
    let mut order: Vec<AgentKind> = Vec::new();
    if let Some(agent) = active {
        order.push(agent);
    }
    if policy.resume_with_earliest {
        for agent in &policy.agent_priority {
            if !order.contains(agent) {
                order.push(*agent);
            }
        }
    }
    order.into_iter().find(|agent| ready(*agent))
}

fn agent_record_ready_for_retry(record: Option<&AgentRecord>, ts: DateTime<Utc>) -> bool {
    match record {
        Some(record) if record.availability == AvailabilityState::Limited => {
            record.reset_at.is_some_and(|reset_at| reset_at <= ts)
        }
        _ => true,
    }
}

fn scheduler_start_error_is_retryable(error: &str) -> bool {
    error.contains("already running") || error.contains("maximum concurrent")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scheduler_delay_tracks_earliest_reset_deadline() {
        let core = crate::test_core().await;

        // No deadline: safety-net interval.
        assert_eq!(core.next_scheduler_delay(), SCHEDULER_INTERVAL);

        // A near deadline shortens the sleep (plus the post-reset buffer).
        core.note_limit_reset_deadline(now() + chrono::Duration::seconds(5));
        let delay = core.next_scheduler_delay();
        assert!(delay <= Duration::from_secs(6), "delay was {delay:?}");
        assert!(delay >= Duration::from_secs(4), "delay was {delay:?}");

        // A later deadline must not displace the earlier one.
        core.note_limit_reset_deadline(now() + chrono::Duration::seconds(600));
        let delay = core.next_scheduler_delay();
        assert!(delay <= Duration::from_secs(6), "delay was {delay:?}");

        // An earlier deadline wins.
        core.note_limit_reset_deadline(now() + chrono::Duration::seconds(1));
        let delay = core.next_scheduler_delay();
        assert!(delay <= Duration::from_secs(2), "delay was {delay:?}");
    }

    #[tokio::test]
    async fn passed_deadline_is_consumed_and_ticks_immediately() {
        let core = crate::test_core().await;
        core.note_limit_reset_deadline(now() - chrono::Duration::seconds(1));
        assert_eq!(core.next_scheduler_delay(), Duration::ZERO);
        // Consumed: back to the safety net.
        assert_eq!(core.next_scheduler_delay(), SCHEDULER_INTERVAL);
    }

    fn record(availability: AvailabilityState, reset_at: Option<DateTime<Utc>>) -> AgentRecord {
        AgentRecord {
            kind: AgentKind::ClaudeCode,
            install_status: "installed".to_string(),
            version: None,
            availability,
            reset_at,
            last_checked: None,
            limit_strikes: 0,
        }
    }

    #[test]
    fn limited_agent_waits_until_reset_time() {
        let ts = Utc::now();
        let future = record(
            AvailabilityState::Limited,
            Some(ts + chrono::Duration::minutes(5)),
        );
        let past = record(
            AvailabilityState::Limited,
            Some(ts - chrono::Duration::minutes(1)),
        );

        assert!(!agent_record_ready_for_retry(Some(&future), ts));
        assert!(agent_record_ready_for_retry(Some(&past), ts));
    }

    #[test]
    fn limited_agent_without_reset_time_does_not_retry() {
        let limited = record(AvailabilityState::Limited, None);

        assert!(!agent_record_ready_for_retry(Some(&limited), Utc::now()));
    }

    #[test]
    fn non_limited_or_missing_agent_record_can_be_probed_again() {
        let available = record(AvailabilityState::Available, None);
        let unknown = record(AvailabilityState::Unknown, None);
        let ts = Utc::now();

        assert!(agent_record_ready_for_retry(Some(&available), ts));
        assert!(agent_record_ready_for_retry(Some(&unknown), ts));
        assert!(agent_record_ready_for_retry(None, ts));
    }

    fn status(kind: AgentKind, availability: AvailabilityState) -> am_proto::AgentStatus {
        am_proto::AgentStatus {
            kind,
            installed: true,
            authenticated: true,
            version: None,
            binary_path: None,
            availability,
            reset_at: None,
            last_checked: None,
        }
    }

    fn thread(active_agent: AgentKind) -> am_proto::AgentThread {
        am_proto::AgentThread {
            id: "t1".into(),
            project_id: Some("p1".into()),
            group_id: None,
            title: "thread".into(),
            status: am_proto::TaskStatus::WaitingForLimit,
            active_agent: Some(active_agent),
            preferred_agent: Some(active_agent),
            permission: "workspace_write".into(),
            execution_backend: am_proto::ExecutionBackend::Host,
            model: None,
            reasoning: None,
            local_provider: None,
            local_base_url: None,
            model_target: am_proto::ModelTargetKind::FrontierDefault,
            compute_lease_id: None,
            compute_provider: None,
            estimated_compute_cost_usd: None,
            fallback_model_target: None,
            original_agent: None,
            fallback_agent: None,
            original_model: None,
            fallback_model: None,
            original_local_provider: None,
            fallback_local_provider: None,
            original_local_base_url: None,
            fallback_local_base_url: None,
            switch_back_pending: false,
            limit_reset_at: None,
            switch_back: true,
            handoff_state: "waiting_for_reset".into(),
            objective: "do work".into(),
            decisions: String::new(),
            progress: String::new(),
            open_questions: String::new(),
            next_actions: String::new(),
            task_budget: am_proto::TaskBudget::default(),
            sort_order: 0,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn thread_limit_resume_waits_for_original_agent_when_earliest_disabled() {
        let policy = am_proto::LimitPolicy {
            resume_with_earliest: false,
            agent_priority: vec![AgentKind::Codex, AgentKind::ClaudeCode],
            ..Default::default()
        };
        let statuses = [
            status(AgentKind::ClaudeCode, AvailabilityState::Limited),
            status(AgentKind::Codex, AvailabilityState::Available),
        ];

        assert_eq!(
            thread_resume_agent(&thread(AgentKind::ClaudeCode), &policy, &statuses),
            None
        );
    }

    #[test]
    fn thread_limit_resume_can_use_first_recovered_agent_when_enabled() {
        let policy = am_proto::LimitPolicy {
            resume_with_earliest: true,
            agent_priority: vec![AgentKind::Codex, AgentKind::ClaudeCode],
            ..Default::default()
        };
        let statuses = [
            status(AgentKind::ClaudeCode, AvailabilityState::Limited),
            status(AgentKind::Codex, AvailabilityState::Available),
        ];

        assert_eq!(
            thread_resume_agent(&thread(AgentKind::ClaudeCode), &policy, &statuses),
            Some(AgentKind::Codex)
        );
    }
}
