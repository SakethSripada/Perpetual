//! Cloud continuation: hand an active thread to provider-hosted execution
//! (Codex Cloud / Claude Code on the web) when the machine is going away,
//! monitor the cloud leg, and reclaim the results into the local worktree.
//!
//! Cloud runs share the account's normal rate limits, so this is never used
//! to dodge a usage limit — that stays with the existing fallback machinery.
//! The contract with the rest of the app: a failed or impossible cloud
//! handoff always degrades to the existing pause/auto-resume path, never to
//! blocked work.

use std::path::{Path, PathBuf};

use am_agents::cloud::{cloud_client_for, CloudError, CloudLaunchRequest, CloudPollStatus};
use am_proto::{
    AgentKind, AppEvent, ApprovalAsk, ApprovalDecision, ApprovalKind, AvailabilityState,
    CloudAvailability, CloudHandoffTrigger, CloudPolicy, CloudRun, CloudRunStatus, TaskStatus,
};
use serde_json::json;

use crate::approvals::ApprovalScope;
use crate::{AppCore, CoreError};

const CLOUD_POLICY_KEY: &str = "cloud_policy";
/// Ceiling on the composed continuation prompt.
const MAX_CLOUD_PROMPT_CHARS: usize = 8_000;
/// How long to wait for a cancelled local session to release its worktree.
const CANCEL_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Machine lifecycle signals that can trigger a cloud handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    SleepImminent,
    ShutdownImminent,
}

/// What to do for one thread when the machine is going away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloudDecision {
    /// Launch a cloud continuation with this provider.
    Launch(AgentKind),
    /// Leave the thread to the existing pause / resume-on-wake path.
    Pause,
    /// Cloud continuation is disabled for this trigger.
    Disabled,
}

/// Pure trigger matrix, kept free of I/O for unit testing.
///
/// `current_cloud` / `other_cloud` are the availability probes for the
/// thread's agent and the opposite provider. Usage limits show up as
/// `ready == false` with a limit blocker, so a limited provider naturally
/// falls through to cross-provider or pause.
pub(crate) fn cloud_decision(
    trigger: CloudHandoffTrigger,
    policy: &CloudPolicy,
    current_agent: AgentKind,
    current_cloud: Option<&CloudAvailability>,
    other_cloud: Option<&CloudAvailability>,
    active_cloud_runs: usize,
) -> CloudDecision {
    if !policy.enabled {
        return CloudDecision::Disabled;
    }
    match trigger {
        CloudHandoffTrigger::Sleep if !policy.continue_on_sleep => return CloudDecision::Disabled,
        CloudHandoffTrigger::Shutdown if !policy.continue_on_shutdown => {
            return CloudDecision::Disabled
        }
        _ => {}
    }
    if active_cloud_runs >= policy.max_concurrent_cloud_runs as usize {
        return CloudDecision::Pause;
    }
    if current_cloud.is_some_and(|a| a.ready) {
        return CloudDecision::Launch(current_agent);
    }
    if policy.allow_cross_provider {
        let ready = [current_cloud, other_cloud]
            .into_iter()
            .flatten()
            .filter(|availability| availability.ready)
            .collect::<Vec<_>>();
        for preferred in &policy.provider_priority {
            if ready
                .iter()
                .any(|availability| availability.agent == *preferred)
            {
                return CloudDecision::Launch(*preferred);
            }
        }
        if let Some(first_ready) = ready.first() {
            return CloudDecision::Launch(first_ready.agent);
        }
    }
    CloudDecision::Pause
}

impl AppCore {
    /// Best-effort shutdown preparation for clients that are about to tear down
    /// the daemon. VS Code has no reliable sleep hook, so shutdown runs the
    /// power-event handoff matrix; each handoff quiesces and checkpoints its
    /// worktree before launching cloud execution.
    pub async fn prepare_shutdown(&self) -> Result<(), CoreError> {
        self.handle_power_event(PowerEvent::ShutdownImminent).await;
        Ok(())
    }

    pub async fn get_cloud_policy(&self) -> Result<CloudPolicy, CoreError> {
        let policy = am_db::repos::settings::get(&self.db.pool, CLOUD_POLICY_KEY)
            .await?
            .and_then(|raw| serde_json::from_str::<CloudPolicy>(&raw).ok())
            .unwrap_or_default();
        Ok(normalize_cloud_policy(policy))
    }

    pub async fn set_cloud_policy(&self, policy: CloudPolicy) -> Result<CloudPolicy, CoreError> {
        let policy = normalize_cloud_policy(policy);
        let value = serde_json::to_string(&policy).map_err(|e| CoreError::Other(e.to_string()))?;
        am_db::repos::settings::set(&self.db.pool, CLOUD_POLICY_KEY, &value).await?;
        self.wake_scheduler();
        Ok(policy)
    }

    /// Probe both providers' cloud readiness.
    pub async fn cloud_availability(&self) -> Result<Vec<CloudAvailability>, CoreError> {
        let policy = self.get_cloud_policy().await.unwrap_or_default();
        let env_id = policy.codex_env_id.as_deref();
        let mut out = Vec::new();
        for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
            if let Some(client) = cloud_client_for(agent) {
                let mut availability = client.availability(env_id).await;
                if let Some(record) = am_db::repos::agent::get(&self.db.pool, agent).await? {
                    if record.availability == AvailabilityState::Limited
                        && record
                            .reset_at
                            .is_none_or(|reset_at| reset_at > am_proto::now())
                    {
                        availability.ready = false;
                        availability.blockers.push(
                            record
                                .reset_at
                                .map(|reset_at| {
                                    format!("{} is usage-limited until {reset_at}", agent.label())
                                })
                                .unwrap_or_else(|| format!("{} is usage-limited", agent.label())),
                        );
                    }
                }
                out.push(availability);
            }
        }
        Ok(out)
    }

    pub async fn list_cloud_runs(&self, thread_id: &str) -> Result<Vec<CloudRun>, CoreError> {
        Ok(am_db::repos::cloud_run::list_for_thread(&self.db.pool, thread_id).await?)
    }

    /// Hand a thread's work to the provider cloud. On any failure the thread
    /// is left in (or returned to) a state the existing pause/resume machinery
    /// already handles, and the error describes why.
    pub async fn start_thread_cloud_handoff(
        &self,
        thread_id: &str,
        trigger: CloudHandoffTrigger,
        agent_override: Option<AgentKind>,
    ) -> Result<CloudRun, CoreError> {
        // A sleep notification can race a manual handoff or shutdown. Hold a
        // process-wide launch gate across the check, checkpoint, provider
        // launch, and DB transition so one thread cannot be handed off twice.
        let _handoff_guard = self.cloud_handoff_lock.lock().await;
        let policy = self.get_cloud_policy().await?;
        if !policy.enabled {
            return Err(CoreError::Other("cloud continuation is disabled".into()));
        }
        let mut thread = am_db::repos::agent_thread::get(&self.db.pool, thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if am_db::repos::cloud_run::active_for_thread(&self.db.pool, thread_id)
            .await?
            .is_some()
        {
            return Err(CoreError::Other(
                "a cloud run is already active for this thread".into(),
            ));
        }
        let active = am_db::repos::cloud_run::list_active(&self.db.pool).await?;
        if active.len() >= policy.max_concurrent_cloud_runs as usize {
            return Err(CoreError::Other(format!(
                "cloud run cap reached ({} active)",
                active.len()
            )));
        }

        let agent = agent_override
            .or(thread.active_agent)
            .or(thread.preferred_agent)
            .ok_or_else(|| CoreError::Other("thread has no agent to continue with".into()))?;
        let client = cloud_client_for(agent)
            .ok_or_else(|| CoreError::Other(format!("{} has no cloud offering", agent.label())))?;
        let availability = client.availability(policy.codex_env_id.as_deref()).await;
        if !availability.ready {
            return Err(CoreError::Other(format!(
                "{} cloud is not ready: {}",
                agent.label(),
                availability.blockers.join("; ")
            )));
        }
        if let Some(record) = am_db::repos::agent::get(&self.db.pool, agent).await? {
            if record.availability == AvailabilityState::Limited
                && record
                    .reset_at
                    .is_none_or(|reset_at| reset_at > am_proto::now())
            {
                return Err(CoreError::Other(
                    record
                        .reset_at
                        .map(|reset_at| {
                            format!("{} cloud is usage-limited until {reset_at}", agent.label())
                        })
                        .unwrap_or_else(|| format!("{} cloud is usage-limited", agent.label())),
                ));
            }
        }

        // Policy engine preflight with the Cloud runtime: deny / approval /
        // budget rules scoped to cloud execution apply here.
        let links =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, thread_id).await?;
        let (worktree, branch) = primary_worktree(&links).ok_or_else(|| {
            CoreError::Other("thread has no local worktree to hand off from".into())
        })?;
        self.policy_preflight(crate::policy::PolicyPreflightInput {
            agent,
            model: thread.model.clone(),
            runtime: am_proto::ExecutionBackend::Cloud,
        })
        .await?;

        if policy.require_approval && trigger == CloudHandoffTrigger::Manual {
            let decision = self
                .request_approval(
                    ApprovalScope {
                        project_id: thread.project_id.clone(),
                        work_node_id: None,
                        task_id: None,
                        thread_id: Some(thread_id.to_string()),
                        session_id: None,
                    },
                    agent,
                    ApprovalAsk {
                        kind: ApprovalKind::Tool,
                        tool_name: "cloud_handoff".into(),
                        command: None,
                        cwd: Some(worktree.to_string_lossy().to_string()),
                        input: json!({ "trigger": trigger.as_str(), "agent": agent.as_str() }),
                        reason: Some(format!(
                            "Continue \"{}\" on {} cloud",
                            thread.title,
                            agent.label()
                        )),
                    },
                )
                .await;
            if !matches!(
                decision,
                ApprovalDecision::Allow | ApprovalDecision::AllowForSession
            ) {
                return Err(CoreError::Other("cloud handoff was not approved".into()));
            }
        }

        // Stop the local session (if any) so the worktree is quiescent, then
        // freshen the handoff files the cloud agent will read.
        if self.sessions.is_active(thread_id).await {
            self.sessions.cancel(thread_id).await;
            if !self
                .sessions
                .wait_until_inactive(thread_id, CANCEL_SETTLE)
                .await
            {
                return Err(CoreError::Other(
                    "the local session did not stop before cloud handoff timed out".into(),
                ));
            }
        }
        // Reload: the session-end handler may have updated progress/handoff.
        if let Some(fresh) = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await? {
            thread = fresh;
        }
        let workspace = self.thread_workspace_path(thread_id, thread.execution_backend);
        self.render_thread_context_files(&thread, &workspace)
            .await?;

        // Checkpoint: commit everything (context files included — they are the
        // handoff payload) and push the branch the cloud will clone.
        let auth = crate::github::github_push_header();
        let base_commit = {
            let wt = worktree.clone();
            tokio::task::spawn_blocking(move || am_vcs::head_sha(&wt))
                .await
                .map_err(|e| CoreError::Other(e.to_string()))?
                .ok()
        };
        let launch_commit = {
            let wt = worktree.clone();
            let branch = branch.clone();
            let auth = auth.clone();
            tokio::task::spawn_blocking(move || -> Result<String, am_vcs::VcsError> {
                am_vcs::commit_all(&wt, "am: cloud handoff checkpoint")?;
                am_vcs::push_branch(&wt, &branch, auth.as_deref())?;
                am_vcs::head_sha(&wt)
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?
            .map_err(|e| {
                CoreError::Other(format!(
                    "could not push checkpoint before cloud handoff: {e}"
                ))
            })?
        };

        let prompt = continuation_prompt(&thread, agent);
        let launch = CloudLaunchRequest {
            prompt,
            worktree: worktree.clone(),
            branch: Some(branch.clone()),
            env_id: policy.codex_env_id.clone(),
        };
        let task_ref = match client.launch(&launch).await {
            Ok(task_ref) => task_ref,
            Err(err) => {
                if let CloudError::UsageLimited { reset_at } = &err {
                    let _ = self.mark_agent_limited(agent, *reset_at).await;
                }
                let msg = match &err {
                    CloudError::UsageLimited { reset_at } => format!(
                        "{} cloud is usage-limited{}",
                        agent.label(),
                        reset_at
                            .map(|dt| format!(" (resets {dt})"))
                            .unwrap_or_default()
                    ),
                    other => other.to_string(),
                };
                return Err(CoreError::Other(msg));
            }
        };

        let run = am_db::repos::cloud_run::create(
            &self.db.pool,
            am_db::repos::cloud_run::NewCloudRun {
                thread_id,
                agent_kind: agent,
                provider_task_id: task_ref.task_id.as_deref(),
                url: task_ref.url.as_deref(),
                env_id: task_ref.env_id.as_deref(),
                branch: Some(&branch),
                base_commit: base_commit.as_deref(),
                launch_commit: Some(&launch_commit),
                trigger,
            },
        )
        .await?;

        thread.status = TaskStatus::RunningInCloud;
        thread.active_agent = Some(agent);
        thread.handoff_state = "cloud_active".to_string();
        let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
        self.events
            .publish(AppEvent::AgentThreadUpdated(saved.clone()));
        self.events.publish(AppEvent::CloudRunUpdated(run.clone()));
        self.activity(
            saved.project_id.clone(),
            None,
            "thread.cloud_handoff_started",
            json!({
                "thread_id": thread_id,
                "agent": agent.as_str(),
                "trigger": trigger.as_str(),
                "cloud_run_id": run.id,
                "url": run.url,
                "branch": branch,
            }),
        )
        .await?;
        self.wake_scheduler();
        Ok(run)
    }

    /// Scheduler tick: poll every active cloud run, observe branch progress,
    /// and reclaim finished/stalled/failed runs.
    pub(crate) async fn run_cloud_monitor_tick(&self) -> Result<(), CoreError> {
        let runs = am_db::repos::cloud_run::list_active(&self.db.pool).await?;
        if runs.is_empty() {
            return Ok(());
        }
        let policy = self.get_cloud_policy().await.unwrap_or_default();
        let poll_interval = std::time::Duration::from_secs(policy.monitor_poll_secs.max(1));
        for run in runs {
            let due = {
                let mut marks = self.cloud_monitor_marks.lock().unwrap();
                let now = std::time::Instant::now();
                match marks.get(&run.id) {
                    Some(last) if now.duration_since(*last) < poll_interval => false,
                    _ => {
                        marks.insert(run.id.clone(), now);
                        true
                    }
                }
            };
            if !due {
                continue;
            }
            if let Err(err) = self.monitor_cloud_run(&run, &policy).await {
                let _ = self
                    .activity(
                        None,
                        None,
                        "thread.cloud_monitor_failed",
                        json!({ "cloud_run_id": run.id, "error": err.to_string() }),
                    )
                    .await;
            }
        }
        Ok(())
    }

    async fn monitor_cloud_run(
        &self,
        run: &CloudRun,
        policy: &CloudPolicy,
    ) -> Result<(), CoreError> {
        let client = cloud_client_for(run.agent_kind)
            .ok_or_else(|| CoreError::Other("no cloud client".into()))?;
        let task_ref = am_agents::cloud::CloudTaskRef {
            agent: run.agent_kind,
            task_id: run.provider_task_id.clone(),
            url: run.url.clone(),
            env_id: run.env_id.clone(),
        };

        // Git observation: has the cloud pushed new commits to the branch?
        let mut saw_progress = false;
        if let (Some(branch), Some(worktree)) =
            (run.branch.as_deref(), self.run_worktree(run).await)
        {
            let auth = crate::github::github_push_header();
            let branch = branch.to_string();
            let remote_sha = tokio::task::spawn_blocking(move || {
                am_vcs::remote_branch_sha(&worktree, &branch, auth.as_deref())
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?
            .unwrap_or(None);
            if let Some(sha) = remote_sha {
                if run.last_seen_commit.as_deref() != Some(sha.as_str()) {
                    am_db::repos::cloud_run::record_activity(&self.db.pool, &run.id, Some(&sha))
                        .await?;
                    saw_progress = true;
                }
            }
        }

        let provider_status = client
            .poll(&task_ref)
            .await
            .unwrap_or(CloudPollStatus::Unknown);
        match provider_status {
            CloudPollStatus::Completed => {
                return self.reclaim_thread_from_cloud(run, "completed", None).await;
            }
            CloudPollStatus::Failed(reason) => {
                return self
                    .reclaim_thread_from_cloud(run, "failed", Some(reason))
                    .await;
            }
            CloudPollStatus::Expired => {
                return self
                    .reclaim_thread_from_cloud(run, "expired", Some("environment expired".into()))
                    .await;
            }
            CloudPollStatus::Running | CloudPollStatus::Provisioning => {
                if run.status != CloudRunStatus::Running
                    && provider_status == CloudPollStatus::Running
                {
                    am_db::repos::cloud_run::set_status(
                        &self.db.pool,
                        &run.id,
                        CloudRunStatus::Running,
                    )
                    .await?;
                    saw_progress = true;
                }
                if provider_status == CloudPollStatus::Running && !saw_progress {
                    // Status alone proves liveness; don't let the stall timer
                    // fire while the provider still reports the task running.
                    am_db::repos::cloud_run::record_activity(&self.db.pool, &run.id, None).await?;
                    saw_progress = true;
                }
            }
            CloudPollStatus::Unknown => {}
        }

        if saw_progress {
            if let Ok(Some(updated)) = am_db::repos::cloud_run::get(&self.db.pool, &run.id).await {
                self.events.publish(AppEvent::CloudRunUpdated(updated));
            }
            return Ok(());
        }

        // Stall detection: nothing observed within the window → reclaim so
        // work returns to a runnable local state instead of silently dying.
        let last_signal = run.last_activity_at.unwrap_or(run.launched_at);
        let stalled_for = am_proto::now() - last_signal;
        if stalled_for.num_seconds() >= policy.stall_timeout_secs as i64 {
            am_db::repos::cloud_run::set_status(&self.db.pool, &run.id, CloudRunStatus::Stalled)
                .await?;
            return self
                .reclaim_thread_from_cloud(
                    run,
                    "stalled",
                    Some(format!(
                        "no progress observed for {}s",
                        stalled_for.num_seconds()
                    )),
                )
                .await;
        }
        Ok(())
    }

    /// Manually reclaim a thread's active cloud run (UI action).
    pub async fn reclaim_cloud_run(&self, thread_id: &str) -> Result<(), CoreError> {
        let run = am_db::repos::cloud_run::active_for_thread(&self.db.pool, thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        self.reclaim_thread_from_cloud(&run, "manual", None).await
    }

    /// Pull cloud results into the local worktree and return the thread to a
    /// locally-managed state. Never destroys local state: merge conflicts
    /// leave the branch fetched and the thread in Review with a note.
    async fn reclaim_thread_from_cloud(
        &self,
        run: &CloudRun,
        reason: &str,
        failure_detail: Option<String>,
    ) -> Result<(), CoreError> {
        let client = cloud_client_for(run.agent_kind);
        let worktree = self.run_worktree(run).await;
        let mut summary_parts: Vec<String> = Vec::new();
        let mut merge_conflict = false;

        if let (Some(worktree), Some(branch)) = (worktree.clone(), run.branch.clone()) {
            let auth = crate::github::github_push_header();
            let launch_commit = run.launch_commit.clone();
            let fetch = tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
                am_vcs::fetch_branch(&worktree, &branch, auth.as_deref())
                    .map_err(|e| format!("fetch: {e}"))?;
                let before = am_vcs::head_sha(&worktree).map_err(|e| e.to_string())?;
                match am_vcs::fast_forward_to_fetch_head(&worktree) {
                    Ok(()) => {
                        let after = am_vcs::head_sha(&worktree).map_err(|e| e.to_string())?;
                        if before == after {
                            Ok(Vec::new())
                        } else {
                            am_vcs::commits_between(&worktree, launch_commit.as_deref(), &after)
                                .map_err(|e| e.to_string())
                        }
                    }
                    Err(e) => Err(format!("non-fast-forward: {e}")),
                }
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;

            match fetch {
                Ok(commits) if commits.is_empty() => {
                    summary_parts.push("no new commits from the cloud run".into());
                }
                Ok(commits) => {
                    summary_parts.push(format!("cloud commits merged:\n{}", commits.join("\n")));
                }
                Err(e) => {
                    merge_conflict = true;
                    summary_parts.push(format!(
                        "cloud branch could not be fast-forwarded ({e}); \
                         the remote branch holds the cloud work — resolve manually"
                    ));
                }
            }
        }

        // Provider-side results git can't see (Codex diffs when the cloud env
        // didn't push). Only meaningful for completed runs.
        if reason == "completed" && !merge_conflict {
            if let (Some(client), Some(worktree)) = (client.as_ref(), worktree.as_ref()) {
                let task_ref = am_agents::cloud::CloudTaskRef {
                    agent: run.agent_kind,
                    task_id: run.provider_task_id.clone(),
                    url: run.url.clone(),
                    env_id: run.env_id.clone(),
                };
                match client.fetch_results(&task_ref, worktree).await {
                    Ok(s) if !s.trim().is_empty() => summary_parts.push(s),
                    Ok(_) => {}
                    Err(e) => summary_parts.push(format!("provider result fetch: {e}")),
                }
            }
        }

        let close_status = if merge_conflict || failure_detail.is_some() && reason == "failed" {
            CloudRunStatus::Failed
        } else if reason == "expired" {
            CloudRunStatus::Expired
        } else {
            CloudRunStatus::Reclaimed
        };
        let failure_note = failure_detail
            .clone()
            .or_else(|| merge_conflict.then(|| "merge conflict while reclaiming".to_string()));
        am_db::repos::cloud_run::close(
            &self.db.pool,
            &run.id,
            close_status,
            failure_note.as_deref(),
        )
        .await?;
        self.cloud_monitor_marks.lock().unwrap().remove(&run.id);

        // Fold the cloud leg into the thread's durable handoff record.
        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &run.thread_id).await?
        {
            let mut entry = format!(
                "Cloud leg on {} ({}): {}",
                run.agent_kind.label(),
                reason,
                summary_parts.join("; ")
            );
            if let Some(url) = &run.url {
                entry.push_str(&format!("\nSession: {url}"));
            }
            thread.progress = crate::agent_thread::append_thread_progress(&thread.progress, &entry);
            let queued =
                am_db::repos::queued_turn::list_for_thread(&self.db.pool, &run.thread_id).await?;
            thread.status = if merge_conflict || (reason == "completed" && queued.is_empty()) {
                TaskStatus::Review
            } else {
                // Failure/stall/expiry, or user messages arrived mid-cloud:
                // queue so the scheduler resumes locally (and drains messages).
                TaskStatus::Queued
            };
            thread.handoff_state = "resolved".to_string();
            let workspace = self.thread_workspace_path(&run.thread_id, thread.execution_backend);
            let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
            let _ = self.render_thread_context_files(&saved, &workspace).await;
            self.events.publish(AppEvent::AgentThreadUpdated(saved));
        }

        if let Ok(Some(closed)) = am_db::repos::cloud_run::get(&self.db.pool, &run.id).await {
            self.events.publish(AppEvent::CloudRunUpdated(closed));
        }
        self.activity(
            None,
            None,
            "thread.cloud_reclaimed",
            json!({
                "thread_id": run.thread_id,
                "cloud_run_id": run.id,
                "agent": run.agent_kind.as_str(),
                "trigger": run.trigger.as_str(),
                "url": run.url,
                "reason": reason,
                "detail": failure_note,
            }),
        )
        .await?;
        self.wake_scheduler();
        Ok(())
    }

    /// Machine lifecycle entry point (sleep/shutdown). Attempts a bounded,
    /// best-effort cloud handoff for every actively running thread; anything
    /// that can't go to the cloud is left for the wake-time resume machinery.
    pub async fn handle_power_event(&self, event: PowerEvent) {
        let policy = self.get_cloud_policy().await.unwrap_or_default();
        let trigger = match event {
            PowerEvent::SleepImminent => CloudHandoffTrigger::Sleep,
            PowerEvent::ShutdownImminent => CloudHandoffTrigger::Shutdown,
        };
        let enabled = match trigger {
            CloudHandoffTrigger::Sleep => policy.enabled && policy.continue_on_sleep,
            CloudHandoffTrigger::Shutdown => policy.enabled && policy.continue_on_shutdown,
            CloudHandoffTrigger::Manual => false,
        };
        if !enabled {
            return;
        }
        let Ok(threads) =
            am_db::repos::agent_thread::list_for_status(&self.db.pool, TaskStatus::Running, 16)
                .await
        else {
            return;
        };
        let running: Vec<_> = threads;
        if running.is_empty() {
            return;
        }

        let availability = self.cloud_availability().await.unwrap_or_default();
        let active_runs = am_db::repos::cloud_run::list_active(&self.db.pool)
            .await
            .map(|r| r.len())
            .unwrap_or(0);

        for (i, thread) in running.into_iter().enumerate() {
            let Some(agent) = thread.active_agent.or(thread.preferred_agent) else {
                continue;
            };
            let current = availability.iter().find(|a| a.agent == agent);
            let other = availability.iter().find(|a| a.agent != agent);
            let decision = cloud_decision(trigger, &policy, agent, current, other, active_runs + i);
            let CloudDecision::Launch(target_agent) = decision else {
                let _ = self
                    .activity(
                        thread.project_id.clone(),
                        None,
                        "thread.cloud_handoff_skipped",
                        json!({
                            "thread_id": thread.id,
                            "trigger": trigger.as_str(),
                            "decision": format!("{decision:?}"),
                        }),
                    )
                    .await;
                continue;
            };
            let override_agent = (target_agent != agent).then_some(target_agent);
            if let Err(err) = self
                .start_thread_cloud_handoff(&thread.id, trigger, override_agent)
                .await
            {
                let _ = self
                    .activity(
                        thread.project_id.clone(),
                        None,
                        "thread.cloud_handoff_failed",
                        json!({
                            "thread_id": thread.id,
                            "trigger": trigger.as_str(),
                            "error": err.to_string(),
                        }),
                    )
                    .await;
            }
        }
    }

    /// Background checkpoint: while cloud continuation is armed, keep each
    /// running thread's branch pushed so the sleep-time delta stays tiny.
    pub(crate) async fn run_cloud_checkpoints(&self) -> Result<(), CoreError> {
        let policy = self.get_cloud_policy().await.unwrap_or_default();
        if !policy.enabled || policy.checkpoint_interval_secs == 0 {
            return Ok(());
        }
        let threads =
            am_db::repos::agent_thread::list_for_status(&self.db.pool, TaskStatus::Running, 16)
                .await?;
        for thread in threads {
            let due = {
                let mut marks = self.cloud_checkpoint_marks.lock().unwrap();
                let now = std::time::Instant::now();
                match marks.get(&thread.id) {
                    Some(last)
                        if now.duration_since(*last).as_secs()
                            < policy.checkpoint_interval_secs =>
                    {
                        false
                    }
                    _ => {
                        marks.insert(thread.id.clone(), now);
                        true
                    }
                }
            };
            if !due {
                continue;
            }
            let links =
                am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &thread.id).await?;
            let Some((worktree, branch)) = primary_worktree(&links) else {
                continue;
            };
            let auth = crate::github::github_push_header();
            let thread_id = thread.id.clone();
            tokio::task::spawn_blocking(move || {
                // Best-effort: a checkpoint failing (e.g. no remote) must not
                // disturb the running session.
                match am_vcs::commit_all(&worktree, "am: checkpoint") {
                    Ok(Some(_)) => {
                        if let Err(e) = am_vcs::push_branch(&worktree, &branch, auth.as_deref()) {
                            tracing::debug!(thread_id, error = %e, "cloud checkpoint push failed");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!(thread_id, error = %e, "cloud checkpoint commit failed");
                    }
                }
            });
        }
        Ok(())
    }

    /// The host worktree a cloud run checkpoints from / reclaims into.
    async fn run_worktree(&self, run: &CloudRun) -> Option<PathBuf> {
        let links = am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &run.thread_id)
            .await
            .ok()?;
        primary_worktree(&links).map(|(worktree, _)| worktree)
    }
}

/// A thread can attach several repos; cloud providers work on one. The first
/// repo with a live worktree and branch is the handoff subject.
fn primary_worktree(links: &[am_proto::AgentThreadRepo]) -> Option<(PathBuf, String)> {
    links.iter().find_map(|link| {
        let worktree = link.worktree_path.as_deref()?;
        let branch = link.branch.clone()?;
        let path = Path::new(worktree);
        path.exists().then(|| (path.to_path_buf(), branch))
    })
}

fn normalize_cloud_policy(mut policy: CloudPolicy) -> CloudPolicy {
    policy.max_concurrent_cloud_runs = policy.max_concurrent_cloud_runs.clamp(1, 64);
    policy.checkpoint_interval_secs = policy.checkpoint_interval_secs.min(86_400);
    policy.monitor_poll_secs = policy.monitor_poll_secs.clamp(1, 3_600);
    policy.stall_timeout_secs = policy.stall_timeout_secs.clamp(60, 86_400);
    policy.codex_env_id = policy
        .codex_env_id
        .take()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    let mut priority = Vec::new();
    for agent in policy.provider_priority {
        if matches!(agent, AgentKind::ClaudeCode | AgentKind::Codex) && !priority.contains(&agent) {
            priority.push(agent);
        }
    }
    for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
        if !priority.contains(&agent) {
            priority.push(agent);
        }
    }
    policy.provider_priority = priority;
    policy
}

/// Continuation prompt for the cloud agent. TASK_CONTEXT.md (committed with
/// the checkpoint) carries the full state; the prompt makes reading it and
/// not redoing finished work explicit.
fn continuation_prompt(thread: &am_proto::AgentThread, agent: AgentKind) -> String {
    let push_note = match agent {
        AgentKind::ClaudeCode => {
            " Push your commits to this branch as you go so progress is visible."
        }
        _ => "",
    };
    let mut prompt = format!(
        "Continue work that was running locally through Perpetual and has been handed off to \
         this cloud environment.\n\n\
         Read TASK_CONTEXT.md at the repository root first — it is the authoritative record of \
         the objective, decisions, progress so far, and next actions. Work listed there as \
         completed is done; do not redo it.\n\n\
         Objective: {}\n",
        thread.objective.trim()
    );
    let next = thread.next_actions.trim();
    if !next.is_empty() {
        prompt.push_str(&format!("\nNext actions:\n{next}\n"));
    }
    prompt.push_str(&format!(
        "\nCommit incrementally with clear messages.{push_note}"
    ));
    if prompt.len() > MAX_CLOUD_PROMPT_CHARS {
        prompt.truncate(MAX_CLOUD_PROMPT_CHARS);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avail(agent: AgentKind, ready: bool) -> CloudAvailability {
        CloudAvailability {
            agent,
            ready,
            authenticated: ready,
            blockers: if ready {
                vec![]
            } else {
                vec!["blocked".into()]
            },
            checked_at: am_proto::now(),
        }
    }

    fn policy(enabled: bool) -> CloudPolicy {
        CloudPolicy {
            enabled,
            ..CloudPolicy::default()
        }
    }

    #[test]
    fn disabled_policy_never_launches() {
        let d = cloud_decision(
            CloudHandoffTrigger::Sleep,
            &policy(false),
            AgentKind::ClaudeCode,
            Some(&avail(AgentKind::ClaudeCode, true)),
            Some(&avail(AgentKind::Codex, true)),
            0,
        );
        assert_eq!(d, CloudDecision::Disabled);
    }

    #[test]
    fn sleep_launches_same_provider_when_ready() {
        let d = cloud_decision(
            CloudHandoffTrigger::Sleep,
            &policy(true),
            AgentKind::ClaudeCode,
            Some(&avail(AgentKind::ClaudeCode, true)),
            Some(&avail(AgentKind::Codex, true)),
            0,
        );
        assert_eq!(d, CloudDecision::Launch(AgentKind::ClaudeCode));
    }

    #[test]
    fn limited_provider_falls_to_cross_provider_only_when_allowed() {
        let mut p = policy(true);
        let current = avail(AgentKind::ClaudeCode, false);
        let other = avail(AgentKind::Codex, true);

        let d = cloud_decision(
            CloudHandoffTrigger::Shutdown,
            &p,
            AgentKind::ClaudeCode,
            Some(&current),
            Some(&other),
            0,
        );
        assert_eq!(d, CloudDecision::Pause);

        p.allow_cross_provider = true;
        let d = cloud_decision(
            CloudHandoffTrigger::Shutdown,
            &p,
            AgentKind::ClaudeCode,
            Some(&current),
            Some(&other),
            0,
        );
        assert_eq!(d, CloudDecision::Launch(AgentKind::Codex));
    }

    #[test]
    fn cross_provider_handoff_respects_cloud_priority() {
        let mut p = policy(true);
        p.allow_cross_provider = true;
        p.provider_priority = vec![AgentKind::Codex, AgentKind::ClaudeCode];
        let current = avail(AgentKind::ClaudeCode, false);
        let other = avail(AgentKind::Codex, true);

        let d = cloud_decision(
            CloudHandoffTrigger::Shutdown,
            &p,
            AgentKind::ClaudeCode,
            Some(&current),
            Some(&other),
            0,
        );
        assert_eq!(d, CloudDecision::Launch(AgentKind::Codex));
    }

    #[test]
    fn trigger_toggles_gate_each_path() {
        let mut p = policy(true);
        p.continue_on_sleep = false;
        let d = cloud_decision(
            CloudHandoffTrigger::Sleep,
            &p,
            AgentKind::Codex,
            Some(&avail(AgentKind::Codex, true)),
            None,
            0,
        );
        assert_eq!(d, CloudDecision::Disabled);

        let d = cloud_decision(
            CloudHandoffTrigger::Shutdown,
            &p,
            AgentKind::Codex,
            Some(&avail(AgentKind::Codex, true)),
            None,
            0,
        );
        assert_eq!(d, CloudDecision::Launch(AgentKind::Codex));
    }

    #[test]
    fn cap_forces_pause() {
        let p = policy(true);
        let d = cloud_decision(
            CloudHandoffTrigger::Sleep,
            &p,
            AgentKind::Codex,
            Some(&avail(AgentKind::Codex, true)),
            None,
            p.max_concurrent_cloud_runs as usize,
        );
        assert_eq!(d, CloudDecision::Pause);
    }

    #[test]
    fn decision_matrix_covers_trigger_toggles_and_provider_states() {
        let ready_claude = avail(AgentKind::ClaudeCode, true);
        let ready_codex = avail(AgentKind::Codex, true);
        let blocked_claude = avail(AgentKind::ClaudeCode, false);
        let blocked_codex = avail(AgentKind::Codex, false);

        for trigger in [CloudHandoffTrigger::Sleep, CloudHandoffTrigger::Shutdown] {
            let mut disabled_trigger = policy(true);
            if trigger == CloudHandoffTrigger::Sleep {
                disabled_trigger.continue_on_sleep = false;
            } else {
                disabled_trigger.continue_on_shutdown = false;
            }
            assert_eq!(
                cloud_decision(
                    trigger,
                    &disabled_trigger,
                    AgentKind::ClaudeCode,
                    Some(&ready_claude),
                    Some(&ready_codex),
                    0,
                ),
                CloudDecision::Disabled
            );

            let same_provider = cloud_decision(
                trigger,
                &policy(true),
                AgentKind::ClaudeCode,
                Some(&ready_claude),
                Some(&ready_codex),
                0,
            );
            assert_eq!(same_provider, CloudDecision::Launch(AgentKind::ClaudeCode));

            let no_cross_provider = cloud_decision(
                trigger,
                &policy(true),
                AgentKind::ClaudeCode,
                Some(&blocked_claude),
                Some(&ready_codex),
                0,
            );
            assert_eq!(no_cross_provider, CloudDecision::Pause);

            let mut cross_provider = policy(true);
            cross_provider.allow_cross_provider = true;
            assert_eq!(
                cloud_decision(
                    trigger,
                    &cross_provider,
                    AgentKind::ClaudeCode,
                    Some(&blocked_claude),
                    Some(&ready_codex),
                    0,
                ),
                CloudDecision::Launch(AgentKind::Codex)
            );

            assert_eq!(
                cloud_decision(
                    trigger,
                    &cross_provider,
                    AgentKind::ClaudeCode,
                    Some(&blocked_claude),
                    Some(&blocked_codex),
                    0,
                ),
                CloudDecision::Pause
            );
        }
    }

    #[test]
    fn cloud_policy_normalization_keeps_launch_limits_safe() {
        let mut p = policy(true);
        p.max_concurrent_cloud_runs = 0;
        p.monitor_poll_secs = 0;
        p.stall_timeout_secs = 0;
        p.provider_priority = vec![AgentKind::Gemini, AgentKind::Codex, AgentKind::Codex];
        p.codex_env_id = Some("  env-123  ".into());

        let normalized = normalize_cloud_policy(p);
        assert_eq!(normalized.max_concurrent_cloud_runs, 1);
        assert_eq!(normalized.monitor_poll_secs, 1);
        assert_eq!(normalized.stall_timeout_secs, 60);
        assert_eq!(
            normalized.provider_priority,
            vec![AgentKind::Codex, AgentKind::ClaudeCode]
        );
        assert_eq!(normalized.codex_env_id.as_deref(), Some("env-123"));
    }

    #[test]
    fn continuation_prompt_pins_do_not_redo() {
        let mut thread = sample_thread();
        thread.objective = "Ship the parser".into();
        thread.next_actions = "- fix failing tests".into();
        let p = continuation_prompt(&thread, AgentKind::ClaudeCode);
        assert!(p.contains("TASK_CONTEXT.md"));
        assert!(p.contains("do not redo it"));
        assert!(p.contains("Ship the parser"));
        assert!(p.contains("fix failing tests"));
        assert!(p.contains("Push your commits"));
        let p = continuation_prompt(&thread, AgentKind::Codex);
        assert!(!p.contains("Push your commits"));
    }

    fn sample_thread() -> am_proto::AgentThread {
        am_proto::AgentThread {
            id: "t".into(),
            project_id: None,
            group_id: None,
            title: "T".into(),
            status: TaskStatus::Running,
            active_agent: Some(AgentKind::ClaudeCode),
            preferred_agent: None,
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
            switch_back: false,
            handoff_state: "none".into(),
            objective: String::new(),
            decisions: String::new(),
            progress: String::new(),
            open_questions: String::new(),
            next_actions: String::new(),
            task_budget: am_proto::TaskBudget::default(),
            sort_order: 0,
            created_at: am_proto::now(),
            updated_at: am_proto::now(),
        }
    }
}
