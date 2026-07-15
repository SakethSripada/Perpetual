//! M1 orchestration: connect repos, run/stop agent sessions in isolated
//! worktrees, stream normalized events, persist the transcript, and compute
//! diffs. All git/process work runs off the async runtime via `spawn_blocking`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use am_agents::{
    binary_version, find_binary, AgentKind, NormalizedEvent, PermissionPolicy, SessionHandle,
    SessionRef, SessionSpec, SessionStatus,
};
use am_db::repos::task_repo::TaskRepoLink;
use am_proto::{
    new_id, now, AgentModelCatalog, AgentModelOption, AgentRunDefaults, AgentStatus, AppEvent,
    ExecutionBackend, GitAutomation, ModelTargetKind, NewLocalRepo, Repo, RepoKind, SessionEvent,
    SessionState, Task, TaskDiff, TaskStatus, TaskUpdate,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::Receiver;

use crate::local_models::{
    legacy_run_target_hash, normalize_model_target, run_target_hash, target_hash_matches,
};
use crate::policy::PolicyPreflightInput;
use crate::sandbox::SandboxLease;
use crate::{AppCore, ApprovalScope, CoreError};

impl AppCore {
    // ---- Repositories ---------------------------------------------------

    /// Validate and connect a local git repository to a project. Connecting a
    /// path that is already connected returns the existing repo rather than
    /// adding a second row for it.
    pub async fn connect_local_repo(&self, input: NewLocalRepo) -> Result<Repo, CoreError> {
        let path = input.path.clone();
        let info = tokio::task::spawn_blocking(move || am_vcs::validate_repo(&path))
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?
            .map_err(|e| CoreError::Other(e.to_string()))?;

        let toplevel = info.toplevel.to_string_lossy().to_string();
        let connected =
            am_db::repos::repo::list_for_project(&self.db.pool, &input.project_id).await?;
        if let Some(existing) = connected.into_iter().find(|repo| {
            repo.local_path
                .as_deref()
                .is_some_and(|existing| same_local_path(existing, &toplevel))
        }) {
            return Ok(existing);
        }

        let repo = am_db::repos::repo::create_local(
            &self.db.pool,
            &input.project_id,
            &info.name,
            &toplevel,
            &info.default_branch,
        )
        .await?;

        self.events.publish(AppEvent::RepoConnected(repo.clone()));
        self.activity(
            Some(repo.project_id.clone()),
            None,
            "repo.connected",
            json!({ "name": repo.name, "path": repo.local_path }),
        )
        .await?;
        Ok(repo)
    }

    pub async fn list_repos(&self, project_id: &str) -> Result<Vec<Repo>, CoreError> {
        Ok(am_db::repos::repo::list_for_project(&self.db.pool, project_id).await?)
    }

    /// Disconnect a repo from its project. Thread assignments referencing it
    /// cascade away; managed clones and worktrees on disk are left alone.
    pub async fn delete_repo(&self, repo_id: &str) -> Result<(), CoreError> {
        let repo = am_db::repos::repo::get(&self.db.pool, repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        am_db::repos::repo::delete(&self.db.pool, repo_id).await?;
        self.activity(
            Some(repo.project_id.clone()),
            None,
            "repo.disconnected",
            json!({ "repo_id": repo.id, "name": repo.name }),
        )
        .await?;
        Ok(())
    }

    /// Disconnect every repo in a project. Returns how many were removed.
    pub async fn clear_project_repos(&self, project_id: &str) -> Result<u64, CoreError> {
        let removed = am_db::repos::repo::delete_for_project(&self.db.pool, project_id).await?;
        if removed > 0 {
            self.activity(
                Some(project_id.to_string()),
                None,
                "repo.cleared",
                json!({ "removed": removed }),
            )
            .await?;
        }
        Ok(removed)
    }

    pub async fn get_task_repo(&self, task_id: &str) -> Result<Option<Repo>, CoreError> {
        let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, task_id).await?
        else {
            return Ok(None);
        };
        Ok(am_db::repos::repo::get(&self.db.pool, &link.repo_id).await?)
    }

    pub async fn assign_task_repo(&self, task_id: &str, repo_id: &str) -> Result<Repo, CoreError> {
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let repo = self
            .validate_project_repo(&task.project_id, repo_id)
            .await?;

        if let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, task_id).await? {
            if link.repo_id == repo.id {
                return Ok(repo);
            }
            if link.worktree_path.is_some() {
                return Err(CoreError::Other(
                    "cannot change repository after a task worktree has been created".into(),
                ));
            }
        }

        am_db::repos::task_repo::replace_repo(&self.db.pool, task_id, &repo.id).await?;
        self.activity(
            Some(task.project_id),
            Some(task.id),
            "task.repo_selected",
            json!({ "repo_id": repo.id, "name": repo.name }),
        )
        .await?;
        Ok(repo)
    }

    // ---- Agents ---------------------------------------------------------

    /// Detect installed/authenticated agents for the settings view.
    pub async fn detect_agents(&self) -> Result<Vec<AgentStatus>, CoreError> {
        let mut out = Vec::new();

        for adapter in self.agents.implemented() {
            let status = self.record_agent_probe(adapter.detect().await).await?;
            out.push(status);
        }

        Ok(out)
    }

    /// Read model/reasoning defaults from each agent's own local configuration.
    pub async fn agent_run_defaults(&self) -> Result<Vec<AgentRunDefaults>, CoreError> {
        let defaults = tokio::task::spawn_blocking(read_agent_run_defaults)
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;
        Ok(defaults)
    }

    /// Detect model choices from the installed provider CLIs/config. This is
    /// intentionally dynamic so shipping a new model does not require an app
    /// release.
    pub async fn agent_model_catalog(&self) -> Result<Vec<AgentModelCatalog>, CoreError> {
        let catalogs = tokio::task::spawn_blocking(build_agent_model_catalog)
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;
        Ok(catalogs)
    }

    // ---- Running tasks --------------------------------------------------

    /// Start an agent session for a task in an isolated worktree. Returns the
    /// internal session id; the run proceeds in the background, streaming events.
    pub async fn run_task(
        &self,
        task_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
    ) -> Result<String, CoreError> {
        self.run_task_inner(task_id, agent, permission, None, None)
            .await
    }

    pub async fn run_task_with_backend(
        &self,
        task_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, CoreError> {
        self.run_task_inner(task_id, agent, permission, None, execution_backend)
            .await
    }

    /// As [`run_task`], but with an optional follow-up `message` that becomes the
    /// turn's prompt (used for steering / queued messages within a task).
    pub(crate) async fn run_task_inner(
        &self,
        task_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, CoreError> {
        if self.sessions.is_active(task_id).await {
            return Err(CoreError::Other("task is already running".into()));
        }
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        self.sync_session_capacity().await;
        let permit = match self.sessions.try_acquire(Some(&task.project_id)) {
            Ok(permit) => permit,
            Err(_) => return Err(self.session_capacity_error().await),
        };

        let repo = self.resolve_task_repo(&task).await?;
        let repo_path = repo
            .local_path
            .clone()
            .ok_or_else(|| CoreError::Other("repository has no local path".into()))?;

        let requested_backend = self
            .task_execution_backend(&task, execution_backend)
            .await?;
        let model_target = normalize_model_target(task.model_target, None);
        let policy = self
            .policy_preflight(PolicyPreflightInput {
                agent,
                model: task.model.clone(),
                runtime: requested_backend,
            })
            .await?;
        let agent = policy.agent;
        let backend = policy.runtime;
        let model = policy.model.clone().or_else(|| task.model.clone());

        let adapter = self.agents.get(agent).ok_or_else(|| {
            CoreError::Other(format!("no adapter available for {}", agent.label()))
        })?;
        let local_model = if model_target == ModelTargetKind::RentedCompute {
            return Err(CoreError::Other(
                "rented compute targets are not supported by the VS Code extension".into(),
            ));
        } else {
            None
        };

        if backend == ExecutionBackend::Host {
            let status = match self.fresh_ready_agent_status(agent).await? {
                Some(status) => status,
                None => self.record_agent_probe(adapter.detect().await).await?,
            };
            if !status.installed {
                return Err(CoreError::Other(format!(
                    "{} is not installed or could not be found",
                    agent.label()
                )));
            }
            if !status.authenticated && local_model.is_none() {
                return Err(CoreError::Other(format!(
                    "{} is installed but not authenticated",
                    agent.label()
                )));
            }
        }

        let sandbox_name = (backend == ExecutionBackend::DockerSandbox)
            .then(|| Self::sandbox_name_for("task", task_id));

        let (worktree, _branch, _base) = self
            .ensure_worktree(&task, &repo, &repo_path, backend)
            .await?;
        let context = self.ensure_task_context(&task).await?;
        self.render_task_context_files(&worktree, &context).await?;
        let target_hash = run_target_hash(
            agent,
            model.as_deref(),
            None,
            None,
            None,
            backend,
            model_target,
            task.compute_lease_id.as_deref(),
        );
        let legacy_target_hash = legacy_run_target_hash(agent, model.as_deref(), None, None, None);
        let prior = self
            .latest_resumable_session_ref(task_id, agent, &target_hash, &legacy_target_hash)
            .await?;
        let resumed_agent_session_id = prior.as_ref().map(|p| p.agent_session_id.clone());
        let (runtime, sandbox_lease) = self
            .session_runtime(agent, backend, sandbox_name.clone())
            .await?;
        let work_node_id = am_db::repos::work_graph::get_node_for_task(&self.db.pool, task_id)
            .await?
            .map(|node| node.id);

        // Persist the session and flip the task to Running.
        let session = am_db::repos::session::create(
            &self.db.pool,
            task_id,
            agent,
            backend,
            sandbox_name.as_deref(),
            model.as_deref(),
            None,
            None,
            None,
            model_target,
            task.compute_lease_id.as_deref(),
            task.compute_provider,
            task.estimated_compute_cost_usd,
            task.fallback_model_target,
            Some(&target_hash),
            policy.envelope_id.as_deref(),
        )
        .await?;
        let task = am_db::repos::task::update(
            &self.db.pool,
            task_id,
            TaskUpdate {
                status: Some(TaskStatus::Running),
                primary_agent: Some(agent),
                ..Default::default()
            },
        )
        .await?;
        self.events.publish(AppEvent::TaskUpdated(task.clone()));
        self.activity(
            Some(task.project_id.clone()),
            Some(task_id.to_string()),
            "session.started",
            json!({
                "agent": agent.as_str(),
                "session_id": session.id,
                "resumed_agent_session_id": resumed_agent_session_id,
            }),
        )
        .await?;

        let spec = SessionSpec {
            worktree,
            prompt: match (&message, prior.is_some()) {
                (Some(msg), true) => build_followup_prompt(msg),
                (Some(msg), false) => format!("{}\n\n{}", build_prompt(&task), msg),
                (None, true) => build_resume_prompt(&task),
                (None, false) => build_prompt(&task),
            },
            model,
            reasoning: None,
            local_model,
            permission,
            runtime,
            policy: Some(policy.runtime_policy.clone()),
            approver: self.approver_for(
                permission,
                agent,
                ApprovalScope {
                    project_id: Some(task.project_id.clone()),
                    task_id: Some(task_id.to_string()),
                    work_node_id: work_node_id.clone(),
                    session_id: Some(session.id.clone()),
                    ..Default::default()
                },
            ),
        };

        let handle = match prior {
            Some(prior) => adapter.resume(prior, spec).await,
            None => adapter.start(spec).await,
        };
        let handle = match handle {
            Ok(handle) => handle,
            Err(err) => {
                self.sandboxes.release(sandbox_lease).await;
                return Err(CoreError::Other(err.to_string()));
            }
        };
        let SessionHandle { events, control } = handle;

        self.sessions.register(task_id, control).await;

        let core = self.clone();
        let session_id = session.id.clone();
        let tid = task_id.to_string();
        let project_id = task.project_id.clone();
        tokio::spawn(async move {
            core.consume_session(
                session_id,
                tid,
                project_id,
                agent,
                permission,
                events,
                permit,
                sandbox_lease,
            )
            .await;
        });

        Ok(session.id)
    }

    /// Stop a running task's session.
    pub async fn stop_task(&self, task_id: &str) -> Result<(), CoreError> {
        if !self.sessions.cancel(task_id).await {
            return Err(CoreError::Other("task is not running".into()));
        }
        Ok(())
    }

    /// Delete a task and everything it owns. Stops any active run first, removes
    /// the work-graph node that fronts it (the FK is SET NULL, so it would
    /// otherwise be orphaned), then deletes the task row — task_repos, sessions,
    /// and turns cascade away with it.
    pub async fn delete_task(&self, task_id: &str) -> Result<(), CoreError> {
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if self.sessions.is_active(task_id).await {
            let _ = self.sessions.cancel(task_id).await;
        }
        if let Some(node) =
            am_db::repos::work_graph::get_node_for_task(&self.db.pool, task_id).await?
        {
            am_db::repos::work_graph::delete_node(&self.db.pool, &node.id).await?;
        }
        let _ = am_db::repos::work_graph::release_locks_for_task(&self.db.pool, task_id).await;
        am_db::repos::task::delete(&self.db.pool, task_id).await?;
        self.activity(
            Some(task.project_id),
            Some(task_id.to_string()),
            "task.deleted",
            json!({ "task_id": task_id }),
        )
        .await?;
        Ok(())
    }

    /// Drain a session's normalized event stream: persist + broadcast each event,
    /// and apply session/task state transitions. Holds the concurrency permit
    /// until the stream closes.
    #[allow(clippy::too_many_arguments)]
    async fn consume_session(
        &self,
        session_id: String,
        task_id: String,
        project_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
        mut events: Receiver<NormalizedEvent>,
        permit: crate::SessionPermit,
        sandbox_lease: Option<SandboxLease>,
    ) {
        let mut saw_usage_limit = false;
        let mut saw_network_loss = false;
        let mut saw_approval_needed = false;
        let mut completed_ok = false;
        let mut limit_reset_at = None;
        let usage_session = am_db::repos::session::get(&self.db.pool, &session_id)
            .await
            .ok()
            .flatten();
        let usage_model = usage_session
            .as_ref()
            .and_then(|session| session.model.clone());
        let usage_policy_envelope_id = usage_session
            .as_ref()
            .and_then(|session| session.policy_envelope_id.clone());

        while let Some(event) = events.recv().await {
            let ended_status = match &event {
                NormalizedEvent::SessionEnded { status } => Some(*status),
                _ => None,
            };

            match &event {
                NormalizedEvent::AwaitingApproval { .. } => saw_approval_needed = true,
                NormalizedEvent::TokenUsage { input, output } => {
                    let _ = self
                        .record_token_usage(
                            Some(project_id.clone()),
                            Some(session_id.clone()),
                            Some(session_id.clone()),
                            agent,
                            usage_model.clone(),
                            usage_policy_envelope_id.clone(),
                            *input,
                            *output,
                        )
                        .await;
                }
                NormalizedEvent::SessionStarted {
                    session_id: provider,
                } => {
                    let _ = am_db::repos::session::set_agent_session_id(
                        &self.db.pool,
                        &session_id,
                        provider,
                    )
                    .await;
                }
                NormalizedEvent::UsageLimitReached { reset_at } => {
                    saw_usage_limit = true;
                    limit_reset_at = *reset_at;
                    let _ = self.mark_agent_limited(agent, *reset_at).await;
                    if let Ok(task) = am_db::repos::task::update(
                        &self.db.pool,
                        &task_id,
                        TaskUpdate {
                            status: Some(TaskStatus::WaitingForLimit),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        self.events.publish(AppEvent::TaskUpdated(task));
                    }
                    let _ = self
                        .activity(
                            Some(project_id.clone()),
                            Some(task_id.clone()),
                            "agent.limited",
                            json!({
                                "agent": agent.as_str(),
                                "reset_at": reset_at,
                            }),
                        )
                        .await;
                }
                NormalizedEvent::NetworkUnavailable { message } => {
                    saw_network_loss = true;
                    if let Ok(task) = am_db::repos::task::update(
                        &self.db.pool,
                        &task_id,
                        TaskUpdate {
                            status: Some(TaskStatus::WaitingForNetwork),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        self.events.publish(AppEvent::TaskUpdated(task));
                    }
                    let _ = self
                        .activity(
                            Some(project_id.clone()),
                            Some(task_id.clone()),
                            "network.unavailable",
                            json!({
                                "agent": agent.as_str(),
                                "message": message,
                            }),
                        )
                        .await;
                }
                NormalizedEvent::SessionEnded { status } => {
                    let effective_status = if saw_network_loss {
                        SessionStatus::Interrupted
                    } else {
                        *status
                    };
                    completed_ok = effective_status == SessionStatus::Completed;
                    let state = match effective_status {
                        SessionStatus::Completed => SessionState::Completed,
                        SessionStatus::Interrupted => SessionState::Interrupted,
                        SessionStatus::Failed => SessionState::Failed,
                    };
                    let _ = am_db::repos::session::finish(&self.db.pool, &session_id, state).await;
                    let _ = am_db::repos::work_graph::finish_runs_for_ref(
                        &self.db.pool,
                        &session_id,
                        state,
                    )
                    .await;

                    if effective_status == SessionStatus::Completed && !saw_usage_limit {
                        let _ = self.mark_agent_available(agent).await;
                        let _ = self
                            .activity(
                                Some(project_id.clone()),
                                Some(task_id.clone()),
                                "agent.available",
                                json!({ "agent": agent.as_str() }),
                            )
                            .await;
                    }

                    let task_status = if saw_network_loss {
                        TaskStatus::WaitingForNetwork
                    } else if saw_usage_limit {
                        TaskStatus::WaitingForLimit
                    } else {
                        match effective_status {
                            // Completed, but actions were blocked by the permission
                            // level — flag for the user to approve (re-run with more
                            // autonomy) rather than silently calling it done.
                            SessionStatus::Completed if saw_approval_needed => {
                                TaskStatus::AwaitingApproval
                            }
                            SessionStatus::Completed => TaskStatus::Review,
                            SessionStatus::Interrupted => TaskStatus::Paused,
                            SessionStatus::Failed => TaskStatus::Failed,
                        }
                    };
                    if let Ok(task) = am_db::repos::task::update(
                        &self.db.pool,
                        &task_id,
                        TaskUpdate {
                            status: Some(task_status),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        self.events.publish(AppEvent::TaskUpdated(task));
                    }

                    if saw_usage_limit {
                        let _ = self
                            .activity(
                                Some(project_id.clone()),
                                Some(task_id.clone()),
                                "agent.limited_session_ended",
                                json!({
                                    "agent": agent.as_str(),
                                    "reset_at": limit_reset_at,
                                    "session_status": effective_status,
                                }),
                            )
                            .await;
                    }
                }
                _ => {}
            }

            let se = map_event(&session_id, &task_id, &event);
            let _ = am_db::repos::message::insert(&self.db.pool, &se).await;
            self.events.publish(AppEvent::Session(se));

            if let Some(status) = ended_status {
                let handoff_status = if saw_network_loss {
                    SessionStatus::Interrupted
                } else {
                    status
                };
                match self
                    .apply_session_handoff(&session_id, &task_id, agent, handoff_status)
                    .await
                {
                    Ok(summary) => {
                        let _ = self
                            .activity(
                                Some(project_id.clone()),
                                Some(task_id.clone()),
                                "context.handoff",
                                json!({
                                    "session_id": session_id.clone(),
                                    "agent": agent.as_str(),
                                    "status": handoff_status,
                                    "summary": summary,
                                }),
                            )
                            .await;
                    }
                    Err(err) => {
                        let _ = self
                            .activity(
                                Some(project_id.clone()),
                                Some(task_id.clone()),
                                "context.handoff_failed",
                                json!({
                                    "session_id": session_id.clone(),
                                    "agent": agent.as_str(),
                                    "status": handoff_status,
                                    "error": err.to_string(),
                                }),
                            )
                            .await;
                    }
                }
            }
        }

        self.sessions.remove(&task_id).await;
        let _ = self
            .activity(
                Some(project_id.clone()),
                Some(task_id.clone()),
                "session.ended",
                json!({ "session_id": session_id }),
            )
            .await;

        let should_fallback = saw_usage_limit;
        let should_wait_for_network = saw_network_loss;
        let reset_at = limit_reset_at;
        // Release any approval still parked on this session so its UI card clears
        // and any awaiting callback unblocks (auto-denied).
        self.cancel_session_approvals(&session_id).await;
        drop(permit);
        self.sandboxes.release(sandbox_lease).await;
        let _ = am_db::repos::work_graph::release_locks_for_task(&self.db.pool, &task_id).await;
        // Capacity and repo locks just freed: queued work can start now.
        self.wake_scheduler();
        self.notify_plan_watchers(&project_id);

        // On a clean completion, optionally auto-commit/push per project settings.
        if completed_ok && !saw_usage_limit {
            self.maybe_auto_git(&task_id, &project_id).await;
        }

        if should_wait_for_network {
            self.handle_task_network_loss(&task_id, &project_id, agent, permission)
                .await;
        } else if should_fallback {
            if let Ok(crate::fallback::FallbackDecision::Switch {
                agent: next_agent, ..
            }) = self
                .apply_fallback_decision(&task_id, &project_id, agent, reset_at)
                .await
            {
                let core = self.clone();
                tokio::spawn(async move {
                    let _ = core
                        .run_task_boxed(&task_id, next_agent, permission, None)
                        .await;
                });
            }
        } else if let Some(next) = self.take_next_message(&task_id).await {
            // Run the next queued follow-up message (steering within the task).
            // Go through the boxed helper to break the async-recursion type cycle
            // (run_task_inner -> consume_session -> run_task_inner).
            let core = self.clone();
            tokio::spawn(async move {
                let _ = core
                    .run_task_boxed(&task_id, next.agent, next.permission, Some(next.message))
                    .await;
            });
        }
    }

    /// A type-erased wrapper around [`Self::run_task_inner`] so it can be called
    /// recursively from `consume_session` without the compiler hitting an opaque
    /// async return-type cycle.
    fn run_task_boxed<'a>(
        &'a self,
        task_id: &'a str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, CoreError>> + Send + 'a>>
    {
        Box::pin(self.run_task_inner(task_id, agent, permission, message, None))
    }

    async fn handle_task_network_loss(
        &self,
        task_id: &str,
        project_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
    ) {
        let policy = self.get_local_model_policy().await.unwrap_or_default();
        if policy.offline_grace_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(policy.offline_grace_secs)).await;
        }

        if !policy.auto_resume_cloud {
            return;
        }
        match self.cloud_connectivity_stable(&policy).await {
            Ok(true) => {
                let _ = self
                    .activity(
                        Some(project_id.to_string()),
                        Some(task_id.to_string()),
                        "network.restored",
                        json!({ "agent": agent.as_str() }),
                    )
                    .await;
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
                let core = self.clone();
                let task_id = task_id.to_string();
                tokio::spawn(async move {
                    let _ = core.run_task_boxed(&task_id, agent, permission, None).await;
                });
            }
            Ok(false) => {
                let _ = self
                    .activity(
                        Some(project_id.to_string()),
                        Some(task_id.to_string()),
                        "network.waiting",
                        json!({ "agent": agent.as_str() }),
                    )
                    .await;
            }
            Err(err) => {
                let _ = self
                    .activity(
                        Some(project_id.to_string()),
                        Some(task_id.to_string()),
                        "network.probe_failed",
                        json!({ "agent": agent.as_str(), "error": err.to_string() }),
                    )
                    .await;
            }
        }
    }

    /// Send a follow-up/steering message to a task's agent. If a session is
    /// already running it is queued and runs when the current turn finishes;
    /// otherwise it starts a new turn now. Returns the new session id, or `None`
    /// when the message was queued.
    pub async fn send_message(
        &self,
        task_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
    ) -> Result<Option<String>, CoreError> {
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err(CoreError::Other("message is empty".into()));
        }
        if self.sessions.is_active(task_id).await {
            self.messages
                .lock()
                .await
                .entry(task_id.to_string())
                .or_default()
                .push_back(crate::QueuedMessage {
                    agent,
                    permission,
                    message,
                });
            let project_id = am_db::repos::task::get(&self.db.pool, task_id)
                .await?
                .map(|t| t.project_id);
            self.activity(
                project_id,
                Some(task_id.to_string()),
                "message.queued",
                json!({}),
            )
            .await?;
            Ok(None)
        } else {
            let id = self
                .run_task_inner(task_id, agent, permission, Some(message), None)
                .await?;
            Ok(Some(id))
        }
    }

    async fn take_next_message(&self, task_id: &str) -> Option<crate::QueuedMessage> {
        let mut map = self.messages.lock().await;
        let queue = map.get_mut(task_id)?;
        let next = queue.pop_front();
        if queue.is_empty() {
            map.remove(task_id);
        }
        next
    }

    // ---- Limit policy ---------------------------------------------------

    pub async fn get_limit_policy(&self) -> Result<am_proto::LimitPolicy, CoreError> {
        let raw = am_db::repos::settings::get(&self.db.pool, LIMIT_POLICY_KEY).await?;
        let policy = raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Ok(normalize_limit_policy(policy))
    }

    pub async fn set_limit_policy(
        &self,
        policy: am_proto::LimitPolicy,
    ) -> Result<am_proto::LimitPolicy, CoreError> {
        let policy = normalize_limit_policy(policy);
        let raw = serde_json::to_string(&policy).unwrap_or_default();
        am_db::repos::settings::set(&self.db.pool, LIMIT_POLICY_KEY, &raw).await?;
        self.activity(
            None,
            None,
            "limit_policy.updated",
            json!({
                "auto_switch": policy.auto_switch,
                "switch_back": policy.switch_back,
                "resume_with_earliest": policy.resume_with_earliest,
                "unknown_reset_retry_secs": policy.unknown_reset_retry_secs,
                "keep_awake": policy.keep_awake,
            }),
        )
        .await?;
        Ok(policy)
    }

    // ---- Git automation -------------------------------------------------

    pub async fn get_git_automation(&self, project_id: &str) -> Result<GitAutomation, CoreError> {
        let raw =
            am_db::repos::settings::get(&self.db.pool, &git_automation_key(project_id)).await?;
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    pub async fn set_git_automation(
        &self,
        project_id: &str,
        settings: GitAutomation,
    ) -> Result<GitAutomation, CoreError> {
        let raw = serde_json::to_string(&settings).unwrap_or_default();
        am_db::repos::settings::set(&self.db.pool, &git_automation_key(project_id), &raw).await?;
        self.activity(
            Some(project_id.to_string()),
            None,
            "git.automation_updated",
            json!({ "auto_commit": settings.auto_commit, "auto_push": settings.auto_push }),
        )
        .await?;
        Ok(settings)
    }

    /// After a completed run, optionally commit (and push) the worktree per the
    /// project's git-automation settings. Best-effort: failures are logged as
    /// activity, never fatal.
    async fn maybe_auto_git(&self, task_id: &str, project_id: &str) {
        let settings = self
            .get_git_automation(project_id)
            .await
            .unwrap_or_default();
        if !settings.auto_commit && !settings.auto_push {
            return;
        }
        let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, task_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(worktree) = link.worktree_path.clone() else {
            return;
        };
        let branch = link.branch.clone();
        let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id)
            .await
            .ok()
            .flatten();
        let has_remote = repo.as_ref().and_then(|r| r.remote_url.as_ref()).is_some();
        let title = am_db::repos::task::get(&self.db.pool, task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.title)
            .unwrap_or_else(|| "agent changes".to_string());

        let mut committed = !settings.auto_commit; // treat "no commit requested" as already satisfied for the push gate
        if settings.auto_commit {
            let wt = worktree.clone();
            let msg = format!("Perpetual: {title}");
            // Exclude the orchestrator-rendered context files from the commit.
            match tokio::task::spawn_blocking(move || {
                am_vcs::commit_all_with_excludes(
                    Path::new(&wt),
                    &msg,
                    &["TASK_CONTEXT.md", "CLAUDE.md", "AGENTS.md"],
                )
            })
            .await
            {
                Ok(Ok(Some(sha))) => {
                    committed = true;
                    let _ = self
                        .activity(
                            Some(project_id.to_string()),
                            Some(task_id.to_string()),
                            "git.committed",
                            json!({ "sha": sha }),
                        )
                        .await;
                }
                Ok(Ok(None)) => {} // nothing to commit
                Ok(Err(e)) => {
                    let _ = self
                        .activity(
                            Some(project_id.to_string()),
                            Some(task_id.to_string()),
                            "git.commit_failed",
                            json!({ "error": e.to_string() }),
                        )
                        .await;
                }
                Err(_) => {}
            }
        }

        if settings.auto_push && committed && has_remote {
            if let Some(branch) = branch {
                let is_github = repo
                    .as_ref()
                    .map(|r| r.kind == RepoKind::GitHub)
                    .unwrap_or(false);
                let auth = if is_github {
                    crate::github::github_push_header()
                } else {
                    None
                };
                let wt = worktree.clone();
                let b = branch.clone();
                match tokio::task::spawn_blocking(move || {
                    am_vcs::push_branch(Path::new(&wt), &b, auth.as_deref())
                })
                .await
                {
                    Ok(Ok(())) => {
                        let _ = self
                            .activity(
                                Some(project_id.to_string()),
                                Some(task_id.to_string()),
                                "git.pushed",
                                json!({ "branch": branch }),
                            )
                            .await;
                    }
                    Ok(Err(e)) => {
                        let _ = self
                            .activity(
                                Some(project_id.to_string()),
                                Some(task_id.to_string()),
                                "git.push_failed",
                                json!({ "error": e.to_string() }),
                            )
                            .await;
                    }
                    Err(_) => {}
                }
            }
        }
    }

    // ---- Transcript + diff ---------------------------------------------

    /// All persisted session events for a task (across sessions), in order.
    pub async fn list_session_events(&self, task_id: &str) -> Result<Vec<SessionEvent>, CoreError> {
        let sessions = am_db::repos::session::list_for_task(&self.db.pool, task_id).await?;
        let mut all = Vec::new();
        for s in sessions {
            let mut evs =
                am_db::repos::message::list_for_session(&self.db.pool, &s.id, task_id).await?;
            all.append(&mut evs);
        }
        Ok(all)
    }

    pub async fn list_sessions(&self, task_id: &str) -> Result<Vec<am_proto::Session>, CoreError> {
        Ok(am_db::repos::session::list_for_task(&self.db.pool, task_id).await?)
    }

    /// Delete a session and its transcript. Refuses to delete the session that
    /// is currently running for its task — stop it first.
    pub async fn delete_session(&self, session_id: &str) -> Result<(), CoreError> {
        let session = am_db::repos::session::get(&self.db.pool, session_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if session.state == SessionState::Running && self.sessions.is_active(&session.task_id).await
        {
            return Err(CoreError::Other(
                "Stop the running session before deleting it.".into(),
            ));
        }
        let project_id = am_db::repos::task::get(&self.db.pool, &session.task_id)
            .await?
            .map(|t| t.project_id);
        am_db::repos::session::delete(&self.db.pool, session_id).await?;
        self.activity(
            project_id,
            Some(session.task_id.clone()),
            "session.deleted",
            json!({ "session_id": session_id, "agent": session.agent_kind.as_str() }),
        )
        .await?;
        Ok(())
    }

    /// Compute the worktree diff for a task against its base commit.
    pub async fn task_diff(&self, task_id: &str) -> Result<TaskDiff, CoreError> {
        let link = match am_db::repos::task_repo::get_for_task(&self.db.pool, task_id).await? {
            Some(l) => l,
            None => return Ok(TaskDiff::default()),
        };
        let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id).await?;
        let (wt, base) = match (link.worktree_path, link.base_ref) {
            (Some(w), Some(b)) => (w, b),
            _ => return Ok(TaskDiff::default()),
        };
        let base_for_diff = base.clone();
        let mut diff = tokio::task::spawn_blocking(move || {
            am_vcs::worktree_diff(Path::new(&wt), &base, am_vcs::MAX_DIFF_BYTES)
        })
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
        .map_err(|e| CoreError::Other(e.to_string()))?;
        diff.repo_id = Some(link.repo_id);
        diff.base_ref = Some(base_for_diff);
        if let Some(repo) = repo {
            diff.repo_name = Some(repo.name);
            diff.remote_url = repo.remote_url;
        }
        Ok(diff)
    }

    // ---- Helpers --------------------------------------------------------

    /// Resolve the repo a task should run against (its linked repo, else the
    /// project's first connected repo).
    async fn resolve_task_repo(&self, task: &Task) -> Result<Repo, CoreError> {
        if let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, &task.id).await? {
            if let Some(repo) = am_db::repos::repo::get(&self.db.pool, &link.repo_id).await? {
                return Ok(repo);
            }
        }
        am_db::repos::repo::list_for_project(&self.db.pool, &task.project_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Other("connect a repository to this project first".into()))
    }

    pub(crate) async fn validate_project_repo(
        &self,
        project_id: &str,
        repo_id: &str,
    ) -> Result<Repo, CoreError> {
        let repo = am_db::repos::repo::get(&self.db.pool, repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if repo.project_id != project_id {
            return Err(CoreError::Other(
                "repository does not belong to this project".into(),
            ));
        }
        Ok(repo)
    }

    async fn task_execution_backend(
        &self,
        task: &Task,
        requested: Option<ExecutionBackend>,
    ) -> Result<ExecutionBackend, CoreError> {
        if let Some(requested) = requested {
            return Ok(requested);
        }
        if let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, &task.id).await? {
            if link.worktree_path.is_some() {
                return Ok(link.workspace_backend);
            }
        }
        let policy = self.get_sandbox_policy().await.unwrap_or_default();
        Ok(policy.default_backend)
    }

    async fn latest_resumable_session_ref(
        &self,
        task_id: &str,
        agent: AgentKind,
        target_hash: &str,
        legacy_target_hash: &str,
    ) -> Result<Option<SessionRef>, CoreError> {
        let sessions = am_db::repos::session::list_for_task(&self.db.pool, task_id).await?;
        Ok(sessions.into_iter().rev().find_map(|session| {
            let target_matches = target_hash_matches(
                session.target_hash.as_deref(),
                target_hash,
                legacy_target_hash,
            );
            if session.agent_kind == agent && target_matches {
                session
                    .agent_session_id
                    .map(|agent_session_id| SessionRef { agent_session_id })
            } else {
                None
            }
        }))
    }

    /// Reuse an existing worktree for the task, or create a fresh one branched
    /// off the repo's current HEAD. Worktrees live under app-data, never inside
    /// the user's repo.
    async fn ensure_worktree(
        &self,
        task: &Task,
        repo: &Repo,
        repo_path: &str,
        backend: ExecutionBackend,
    ) -> Result<(PathBuf, String, String), CoreError> {
        if let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, &task.id).await? {
            if let (Some(wt), Some(base)) = (link.worktree_path.clone(), link.base_ref.clone()) {
                if link.workspace_backend == backend && Path::new(&wt).exists() {
                    return Ok((PathBuf::from(wt), link.branch.unwrap_or_default(), base));
                }
            }
        }

        let short = task.id.split('-').next().unwrap_or("task").to_string();
        let branch = format!("am/task-{short}");
        let worktree = match backend {
            // Cloud legs checkpoint from and reclaim into the host worktree.
            ExecutionBackend::Host | ExecutionBackend::Cloud => {
                self.data_dir.join("worktrees").join(&task.id)
            }
            ExecutionBackend::DockerSandbox => self
                .data_dir
                .join("sandbox-workspaces")
                .join("tasks")
                .join(&task.id),
        };

        let repo_path = repo_path.to_string();
        let wt_clone = worktree.clone();
        let branch_clone = branch.clone();
        let base_ref = tokio::task::spawn_blocking(move || -> Result<String, am_vcs::VcsError> {
            let base = am_vcs::head_sha(Path::new(&repo_path))?;
            match backend {
                ExecutionBackend::Host | ExecutionBackend::Cloud => {
                    am_vcs::create_worktree(
                        Path::new(&repo_path),
                        &wt_clone,
                        &branch_clone,
                        &base,
                    )?;
                }
                ExecutionBackend::DockerSandbox => {
                    am_vcs::create_clone_workspace(
                        Path::new(&repo_path),
                        &wt_clone,
                        &branch_clone,
                        &base,
                    )?;
                }
            }
            Ok(base)
        })
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
        .map_err(|e| CoreError::Other(e.to_string()))?;

        am_db::repos::task_repo::upsert(
            &self.db.pool,
            &TaskRepoLink {
                task_id: task.id.clone(),
                repo_id: repo.id.clone(),
                worktree_path: Some(worktree.to_string_lossy().to_string()),
                branch: Some(branch.clone()),
                base_ref: Some(base_ref.clone()),
                workspace_backend: backend,
            },
        )
        .await?;

        Ok((worktree, branch, base_ref))
    }
}

/// Compare two stored repository paths. Git reports `--show-toplevel` with
/// forward slashes even on Windows, while paths that reach us from the editor
/// use the platform separator, so compare on a normalized key instead of the
/// raw strings.
fn same_local_path(a: &str, b: &str) -> bool {
    local_path_key(a) == local_path_key(b)
}

fn local_path_key(path: &str) -> String {
    let trimmed = path
        .trim()
        .trim_start_matches("\\\\?\\")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed
    }
}

fn normalize_limit_policy(mut policy: am_proto::LimitPolicy) -> am_proto::LimitPolicy {
    policy.unknown_reset_retry_secs = policy.unknown_reset_retry_secs.min(7 * 24 * 60 * 60);
    let mut priority = Vec::new();
    for agent in policy.agent_priority {
        if !priority.contains(&agent) {
            priority.push(agent);
        }
    }
    if priority.is_empty() {
        priority = vec![AgentKind::ClaudeCode, AgentKind::Codex];
    }
    policy.agent_priority = priority;
    policy
}

/// Compose the initial agent prompt from a task.
fn build_prompt(task: &Task) -> String {
    let mut prompt = task.title.clone();
    if let Some(desc) = &task.description {
        if !desc.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(desc);
        }
    }
    prompt.push_str(
        "\n\nBefore making changes, read TASK_CONTEXT.md and the agent-specific context file in this worktree for the current objective, progress, and next actions.",
    );
    prompt
}

fn build_resume_prompt(task: &Task) -> String {
    format!(
        "Continue the Perpetual task \"{}\" in this worktree. Read TASK_CONTEXT.md and the agent-specific context file first, then proceed from the recorded progress and next actions.",
        task.title
    )
}

const LIMIT_POLICY_KEY: &str = "limit_policy";
const MAX_EVENT_DETAIL_CHARS: usize = 2_000;

fn git_automation_key(project_id: &str) -> String {
    format!("git_automation:{project_id}")
}

/// A user follow-up/steering message that continues an existing session.
fn build_followup_prompt(message: &str) -> String {
    message.to_string()
}

fn read_agent_run_defaults() -> Vec<AgentRunDefaults> {
    vec![read_claude_defaults(), read_codex_defaults()]
}

fn build_agent_model_catalog() -> Vec<AgentModelCatalog> {
    let defaults = read_agent_run_defaults();
    let claude = defaults
        .iter()
        .find(|defaults| defaults.kind == AgentKind::ClaudeCode)
        .cloned()
        .unwrap_or(AgentRunDefaults {
            kind: AgentKind::ClaudeCode,
            model: None,
            reasoning: None,
        });
    let codex = defaults
        .iter()
        .find(|defaults| defaults.kind == AgentKind::Codex)
        .cloned()
        .unwrap_or(AgentRunDefaults {
            kind: AgentKind::Codex,
            model: None,
            reasoning: None,
        });
    vec![claude_model_catalog(claude), codex_model_catalog(codex)]
}

/// Claude effort levels for models that support the full range (Fable 5,
/// Opus 4.7+, Sonnet 5).
const CLAUDE_EFFORT_FULL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Claude effort levels for the 4.6-generation models (no `xhigh`).
const CLAUDE_EFFORT_46: &[&str] = &["low", "medium", "high", "max"];

/// The known Claude Code model lineup with versioned ids, display names, the
/// alias each versioned id currently resolves from, and per-model effort
/// support. The installed CLI stays authoritative where it exposes data: the
/// effort list is intersected with the levels advertised by `claude --help`,
/// and any additional aliases/full ids found in the help text are merged in,
/// so newer CLIs surface new models and levels without an extension update.
fn curated_claude_models(cli_levels: Option<&[String]>) -> Vec<AgentModelOption> {
    let entries: &[(&str, &str, &[&str], &[&str])] = &[
        (
            "claude-fable-5",
            "Claude Fable 5",
            &["fable"],
            CLAUDE_EFFORT_FULL,
        ),
        (
            "claude-opus-4-8",
            "Claude Opus 4.8",
            &["opus"],
            CLAUDE_EFFORT_FULL,
        ),
        (
            "claude-opus-4-7",
            "Claude Opus 4.7",
            &[],
            CLAUDE_EFFORT_FULL,
        ),
        ("claude-opus-4-6", "Claude Opus 4.6", &[], CLAUDE_EFFORT_46),
        (
            "claude-sonnet-5",
            "Claude Sonnet 5",
            &["sonnet"],
            CLAUDE_EFFORT_FULL,
        ),
        (
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            &[],
            CLAUDE_EFFORT_46,
        ),
        // Haiku 4.5 does not accept an effort level; only "Default" applies.
        ("claude-haiku-4-5", "Claude Haiku 4.5", &["haiku"], &[]),
    ];
    entries
        .iter()
        .map(|(id, label, aliases, efforts)| AgentModelOption {
            id: id.to_string(),
            label: label.to_string(),
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            family: model_family(id),
            default: false,
            available: true,
            source: "claude_code".to_string(),
            reasoning: efforts
                .iter()
                .filter(|effort| {
                    cli_levels
                        .map(|levels| {
                            levels
                                .iter()
                                .any(|level| level.eq_ignore_ascii_case(effort))
                        })
                        .unwrap_or(true)
                })
                .map(|effort| effort.to_string())
                .collect(),
            default_reasoning: None,
            local_provider: None,
            local_base_url: None,
        })
        .collect()
}

fn claude_model_catalog(defaults: AgentRunDefaults) -> AgentModelCatalog {
    let binary = find_binary("claude");
    let binary_path = binary
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let version = binary.as_ref().and_then(|path| binary_version(path));
    let mut source = "claude_code".to_string();
    let mut error = None;
    let mut cli_levels: Option<Vec<String>> = None;
    let mut help_models = Vec::new();

    if let Some(binary) = binary.as_ref() {
        match command_output_timeout(binary, &["--help"], Duration::from_secs(6)) {
            Ok(help) => {
                source = "claude_help".to_string();
                cli_levels = parse_claude_effort_levels(&help);
                help_models = parse_claude_help_models(&help);
            }
            Err(err) => error = Some(err),
        }
    } else {
        error = Some("claude binary was not found".to_string());
    }

    let mut models = curated_claude_models(cli_levels.as_deref());
    let mut reasoning =
        cli_levels.unwrap_or_else(|| CLAUDE_EFFORT_FULL.iter().map(|s| s.to_string()).collect());
    // Anything the CLI itself mentions (new aliases, new full ids) merges in;
    // aliases already carried by a curated entry dedupe into that entry, and
    // genuinely new models inherit the CLI-advertised effort levels.
    for mut option in help_models {
        let known = models.iter().any(|existing| {
            model_ids_equal(&existing.id, &option.id)
                || existing
                    .aliases
                    .iter()
                    .any(|alias| model_ids_equal(alias, &option.id))
        });
        if !known {
            option.reasoning = reasoning.clone();
        }
        push_model_option(&mut models, option);
    }

    if let Some(model) = defaults.model.as_deref() {
        push_model_option(
            &mut models,
            AgentModelOption {
                id: model.to_string(),
                label: pretty_model_label(model),
                aliases: Vec::new(),
                family: model_family(model),
                default: true,
                available: true,
                source: "settings".to_string(),
                reasoning: reasoning.clone(),
                default_reasoning: defaults.reasoning.clone(),
                local_provider: None,
                local_base_url: None,
            },
        );
        mark_default_model(&mut models, model);
    }
    if let Some(reasoning_default) = defaults.reasoning.as_deref() {
        push_unique(&mut reasoning, reasoning_default);
    }

    AgentModelCatalog {
        agent: AgentKind::ClaudeCode,
        default_model: defaults.model,
        default_reasoning: defaults.reasoning,
        models,
        reasoning,
        binary_path,
        version,
        source,
        detected_at: now(),
        error,
    }
}

fn codex_model_catalog(mut defaults: AgentRunDefaults) -> AgentModelCatalog {
    let binary = find_binary("codex");
    let binary_path = binary
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let version = binary.as_ref().and_then(|path| binary_version(path));
    let mut source = "settings".to_string();
    let mut error = None;
    let mut models = Vec::new();
    let mut reasoning = Vec::new();

    if let Some(binary) = binary.as_ref() {
        // Current Codex CLIs expose the model catalog over the app-server
        // JSON-RPC `model/list` request (the older `codex debug models`
        // subcommand was removed around 0.117). Try the app-server first and
        // keep the legacy paths as fallbacks for older installs.
        match codex_app_server_models(binary, defaults.model.as_deref()) {
            Ok((parsed, parsed_reasoning)) if !parsed.is_empty() => {
                source = "codex_app_server".to_string();
                models = parsed;
                reasoning = parsed_reasoning;
            }
            first_attempt => {
                let app_server_error = match first_attempt {
                    Ok(_) => "codex app-server returned no models".to_string(),
                    Err(err) => err,
                };
                match command_output_timeout(binary, &["debug", "models"], Duration::from_secs(4))
                    .and_then(|raw| parse_codex_debug_models(&raw, defaults.model.as_deref()))
                {
                    Ok((parsed, parsed_reasoning)) => {
                        source = "codex_debug_models".to_string();
                        models = parsed;
                        reasoning = parsed_reasoning;
                    }
                    Err(_) => {
                        error = Some(app_server_error);
                        if defaults.model.is_none() {
                            defaults.model = codex_doctor_default_model(binary);
                        }
                    }
                }
            }
        }
    } else {
        error = Some("codex binary was not found".to_string());
    }

    if let Some(model) = defaults.model.as_deref() {
        push_model_option(
            &mut models,
            AgentModelOption {
                id: model.to_string(),
                label: pretty_model_label(model),
                aliases: Vec::new(),
                family: model_family(model),
                default: true,
                available: true,
                source: "settings".to_string(),
                reasoning: reasoning.clone(),
                default_reasoning: defaults.reasoning.clone(),
                local_provider: None,
                local_base_url: None,
            },
        );
        mark_default_model(&mut models, model);
    }
    if let Some(reasoning_default) = defaults.reasoning.as_deref() {
        push_unique(&mut reasoning, reasoning_default);
    }
    if reasoning.is_empty() {
        reasoning.extend(["low", "medium", "high", "xhigh"].map(str::to_string));
    }

    AgentModelCatalog {
        agent: AgentKind::Codex,
        default_model: defaults.model,
        default_reasoning: defaults.reasoning,
        models,
        reasoning,
        binary_path,
        version,
        source,
        detected_at: now(),
        error,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAppServerModel {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    default_reasoning_effort: Option<String>,
    #[serde(default)]
    supported_reasoning_efforts: Vec<CodexAppServerReasoning>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAppServerReasoning {
    reasoning_effort: String,
}

/// Query the installed Codex CLI for its live model catalog over the
/// app-server JSON-RPC transport (`initialize` → `initialized` →
/// `model/list`). This is how the Codex TUI itself populates its model
/// picker, so new models and their per-model reasoning efforts show up here
/// the moment the CLI updates — no extension release required.
fn codex_app_server_models(
    binary: &Path,
    default_model: Option<&str>,
) -> Result<(Vec<AgentModelOption>, Vec<String>), String> {
    use std::io::{BufRead, BufReader, Write};

    let mut child = Command::new(binary)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("failed to run {} app-server: {err}", binary.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{} app-server did not expose stdin", binary.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{} app-server did not expose stdout", binary.display()))?;

    let request = |id: Option<u64>, method: &str, params: serde_json::Value| {
        let mut message = json!({ "method": method, "params": params });
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        format!("{message}\n")
    };
    let handshake = [
        request(
            Some(1),
            "initialize",
            json!({ "clientInfo": { "name": "Perpetual", "version": env!("CARGO_PKG_VERSION") } }),
        ),
        request(None, "initialized", json!({})),
        request(Some(2), "model/list", json!({ "includeHidden": false })),
    ]
    .concat();
    stdin
        .write_all(handshake.as_bytes())
        .and_then(|_| stdin.flush())
        .map_err(|err| format!("failed to write to {} app-server: {err}", binary.display()))?;

    // Read on a helper thread so the deadline holds even if the server hangs.
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next().transpose() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut outcome = Err(format!("{} app-server timed out", binary.display()));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let line = match line_rx.recv_timeout(remaining) {
            Ok(line) => line,
            Err(_) => break,
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(serde_json::Value::as_u64) != Some(2) {
            continue;
        }
        outcome = if let Some(err) = value.get("error") {
            Err(err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("codex app-server model/list failed")
                .to_string())
        } else {
            Ok(value.pointer("/result/data").cloned().unwrap_or_default())
        };
        break;
    }

    // Close stdin first: the app-server exits on stdin EOF, which also covers
    // the Windows npm `.cmd` shim case where killing the wrapper would leave
    // the real server process holding the stdout pipe open. Never join the
    // reader thread — if the server ignores EOF, it would block forever; the
    // thread exits on its own once the pipe closes.
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    drop(line_rx);
    drop(reader);

    let data = outcome?;
    let parsed: Vec<CodexAppServerModel> = serde_json::from_value(data)
        .map_err(|err| format!("could not parse codex app-server models: {err}"))?;
    Ok(collect_app_server_models(parsed, default_model))
}

fn collect_app_server_models(
    parsed: Vec<CodexAppServerModel>,
    default_model: Option<&str>,
) -> (Vec<AgentModelOption>, Vec<String>) {
    let mut models = Vec::new();
    let mut reasoning = Vec::new();
    for model in parsed {
        if model.hidden {
            continue;
        }
        let id = model.model.as_deref().unwrap_or(&model.id).trim();
        if id.is_empty() {
            continue;
        }
        let model_reasoning: Vec<String> = model
            .supported_reasoning_efforts
            .iter()
            .map(|level| level.reasoning_effort.trim().to_string())
            .filter(|effort| !effort.is_empty())
            .collect();
        for effort in &model_reasoning {
            push_unique(&mut reasoning, effort);
        }
        if let Some(default_effort) = model.default_reasoning_effort.as_deref() {
            push_unique(&mut reasoning, default_effort);
        }
        let is_default = default_model
            .map(|default| model_ids_equal(default, id))
            .unwrap_or(model.is_default);
        push_model_option(
            &mut models,
            AgentModelOption {
                id: id.to_string(),
                label: model
                    .display_name
                    .clone()
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| pretty_model_label(id)),
                aliases: if model.id.trim() != id {
                    vec![model.id.trim().to_string()]
                } else {
                    Vec::new()
                },
                family: model_family(id),
                default: is_default,
                available: true,
                source: "codex_app_server".to_string(),
                reasoning: model_reasoning,
                default_reasoning: model.default_reasoning_effort,
                local_provider: None,
                local_base_url: None,
            },
        );
    }
    (models, reasoning)
}

#[derive(Debug, Deserialize)]
struct CodexDebugModels {
    #[serde(default)]
    models: Vec<CodexDebugModel>,
}

#[derive(Debug, Deserialize)]
struct CodexDebugModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexDebugReasoning>,
}

#[derive(Debug, Deserialize)]
struct CodexDebugReasoning {
    effort: String,
}

fn parse_codex_debug_models(
    raw: &str,
    default_model: Option<&str>,
) -> Result<(Vec<AgentModelOption>, Vec<String>), String> {
    let parsed: CodexDebugModels = serde_json::from_str(raw)
        .map_err(|err| format!("could not parse codex debug models: {err}"))?;
    let mut models = Vec::new();
    let mut reasoning = Vec::new();
    for model in parsed.models {
        let id = model.slug.trim();
        if id.is_empty() {
            continue;
        }
        if model
            .visibility
            .as_deref()
            .is_some_and(|visibility| !visibility.eq_ignore_ascii_case("list"))
        {
            continue;
        }
        let model_reasoning = model
            .supported_reasoning_levels
            .into_iter()
            .map(|level| level.effort)
            .filter(|effort| !effort.trim().is_empty())
            .collect::<Vec<_>>();
        for effort in &model_reasoning {
            push_unique(&mut reasoning, effort);
        }
        if let Some(default_effort) = model.default_reasoning_level.as_deref() {
            push_unique(&mut reasoning, default_effort);
        }
        let is_default = default_model
            .map(|default| model_ids_equal(default, id))
            .unwrap_or(false);
        push_model_option(
            &mut models,
            AgentModelOption {
                id: id.to_string(),
                label: model
                    .display_name
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| pretty_model_label(id)),
                aliases: Vec::new(),
                family: model_family(id),
                default: is_default,
                available: true,
                source: "codex_debug_models".to_string(),
                reasoning: model_reasoning,
                default_reasoning: model.default_reasoning_level,
                local_provider: None,
                local_base_url: None,
            },
        );
    }
    Ok((models, reasoning))
}

fn parse_claude_help_models(help: &str) -> Vec<AgentModelOption> {
    let lower = help.to_lowercase();
    let mut models = Vec::new();
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        if lower.contains(alias) {
            push_model_option(
                &mut models,
                AgentModelOption {
                    id: alias.to_string(),
                    label: pretty_model_label(alias),
                    aliases: Vec::new(),
                    family: Some(alias.to_string()),
                    default: false,
                    available: true,
                    source: "claude_help".to_string(),
                    reasoning: Vec::new(),
                    default_reasoning: None,
                    local_provider: None,
                    local_base_url: None,
                },
            );
        }
    }
    for token in help.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')) {
        let token = token.trim_matches(|ch: char| ch == '-' || ch == '_' || ch == '.');
        if token.len() < "claude-x-1".len() || !token.starts_with("claude-") {
            continue;
        }
        push_model_option(
            &mut models,
            AgentModelOption {
                id: token.to_string(),
                label: pretty_model_label(token),
                aliases: Vec::new(),
                family: model_family(token),
                default: false,
                available: true,
                source: "claude_help".to_string(),
                reasoning: Vec::new(),
                default_reasoning: None,
                local_provider: None,
                local_base_url: None,
            },
        );
    }
    models
}

/// Extract the effort levels the installed CLI advertises. Claude Code's help
/// text documents the flag as `--effort <level>  ... (low, medium, high,
/// xhigh, max)`, so the parenthesized list after `--effort` is authoritative
/// and automatically picks up levels added in future CLI releases. Falls back
/// to scanning for known level names anywhere in the help text.
fn parse_claude_effort_levels(help: &str) -> Option<Vec<String>> {
    if let Some(idx) = help.find("--effort") {
        let window = &help[idx..(idx + 400).min(help.len())];
        if let Some(open) = window.find('(') {
            if let Some(close) = window[open..].find(')') {
                let levels: Vec<String> = window[open + 1..open + close]
                    .split([',', ' ', '\n', '\r'])
                    .map(str::trim)
                    .filter(|token| {
                        !token.is_empty()
                            && token.len() <= 12
                            && token.chars().all(|ch| ch.is_ascii_alphabetic())
                    })
                    .map(str::to_lowercase)
                    .collect();
                if !levels.is_empty() {
                    return Some(levels);
                }
            }
        }
    }
    let lower = help.to_lowercase();
    let found: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| lower.contains(level))
        .map(str::to_string)
        .collect();
    (!found.is_empty()).then_some(found)
}

fn codex_doctor_default_model(binary: &Path) -> Option<String> {
    let raw = command_output_timeout(binary, &["doctor"], Duration::from_secs(3)).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        let lower = line.to_lowercase();
        if !(lower.contains("model") || lower.contains("default")) {
            continue;
        }
        let value = line
            .split([':', '='])
            .nth(1)
            .map(str::trim)
            .unwrap_or(line)
            .split_whitespace()
            .find(|part| {
                let part = part.trim_matches(|ch: char| ch == ',' || ch == ';');
                part.contains("gpt-") || part.starts_with('o') || part.starts_with("codex")
            })?;
        return Some(
            value
                .trim_matches(|ch: char| ch == ',' || ch == ';')
                .to_string(),
        );
    }
    None
}

fn command_output_timeout(
    binary: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run {}: {err}", binary.display()))?;
    // Provider catalogs can be hundreds of KB. Drain both pipes while the
    // process is running; waiting for exit before reading deadlocks once an OS
    // pipe buffer fills (the current Codex catalog is already large enough).
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{} did not expose stdout", binary.display()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{} did not expose stderr", binary.display()))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes);
        (result, bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.read_to_end(&mut bytes);
        (result, bytes)
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout_result, stdout) = stdout_reader
                    .join()
                    .map_err(|_| format!("failed to read {} stdout", binary.display()))?;
                stdout_result
                    .map_err(|err| format!("failed to read {} stdout: {err}", binary.display()))?;
                let (stderr_result, stderr) = stderr_reader
                    .join()
                    .map_err(|_| format!("failed to read {} stderr", binary.display()))?;
                stderr_result
                    .map_err(|err| format!("failed to read {} stderr: {err}", binary.display()))?;
                if !status.success() {
                    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
                    return Err(if stderr.is_empty() {
                        format!("{} exited with {status}", binary.display())
                    } else {
                        stderr
                    });
                }
                return Ok(String::from_utf8_lossy(&stdout).to_string());
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("{} timed out", binary.display()));
            }
            Err(err) => return Err(format!("failed to poll {}: {err}", binary.display())),
        }
    }
}

fn push_model_option(models: &mut Vec<AgentModelOption>, option: AgentModelOption) {
    let key = model_dedupe_key(&option.id);
    if key.is_empty() {
        return;
    }
    // An option matches an existing entry when the ids collide directly or
    // when either side lists the other's id as an alias (e.g. the "opus"
    // alias folding into the versioned "claude-opus-4-8" entry).
    if let Some(existing) = models.iter_mut().find(|existing| {
        model_dedupe_key(&existing.id) == key
            || existing
                .aliases
                .iter()
                .any(|alias| model_dedupe_key(alias) == key)
            || option
                .aliases
                .iter()
                .any(|alias| model_ids_equal(alias, &existing.id))
    }) {
        if option.default {
            existing.default = true;
        }
        if existing.label.trim().is_empty() {
            existing.label = option.label;
        }
        for alias in option.aliases {
            if !existing
                .aliases
                .iter()
                .any(|existing| model_ids_equal(existing, &alias))
            {
                existing.aliases.push(alias);
            }
        }
        if existing.reasoning.is_empty() {
            existing.reasoning = option.reasoning;
        }
        return;
    }
    models.push(option);
}

fn mark_default_model(models: &mut [AgentModelOption], model: &str) {
    for option in models {
        option.default = model_ids_equal(&option.id, model)
            || option
                .aliases
                .iter()
                .any(|alias| model_ids_equal(alias, model));
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(trimmed))
    {
        values.push(trimmed.to_string());
    }
}

fn model_ids_equal(a: &str, b: &str) -> bool {
    model_dedupe_key(a) == model_dedupe_key(b)
}

fn model_dedupe_key(value: &str) -> String {
    value
        .trim()
        .trim_end_matches([']', ')'])
        .split('[')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

fn model_family(value: &str) -> Option<String> {
    let normalized = model_dedupe_key(value);
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("fable") {
        return Some("fable".to_string());
    }
    if normalized.contains("opus") {
        return Some("opus".to_string());
    }
    if normalized.contains("sonnet") {
        return Some("sonnet".to_string());
    }
    if normalized.contains("haiku") {
        return Some("haiku".to_string());
    }
    if normalized.contains("codex") {
        return Some("codex".to_string());
    }
    if normalized.starts_with("gpt-") {
        return Some("gpt".to_string());
    }
    normalized.split(['-', '_', '.']).next().map(str::to_string)
}

fn pretty_model_label(value: &str) -> String {
    let value = model_dedupe_key(value);
    if value.is_empty() {
        return "Default".to_string();
    }
    match value.as_str() {
        "fable" => return "Fable".to_string(),
        "opus" => return "Opus".to_string(),
        "sonnet" => return "Sonnet".to_string(),
        "haiku" => return "Haiku".to_string(),
        _ => {}
    }
    if value.starts_with("gpt-") {
        return value.replacen("gpt", "GPT", 1);
    }
    if value.starts_with("claude-") {
        return value
            .split('-')
            .filter(|part| !part.is_empty())
            .map(|part| {
                if part.chars().all(|ch| ch.is_ascii_digit()) {
                    part.to_string()
                } else {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    value
}

fn read_claude_defaults() -> AgentRunDefaults {
    let settings = home_path(".claude/settings.json")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());

    AgentRunDefaults {
        kind: AgentKind::ClaudeCode,
        model: settings
            .as_ref()
            .and_then(|value| string_field(value, "model")),
        reasoning: settings
            .as_ref()
            .and_then(|value| string_field(value, "effortLevel")),
    }
}

fn read_codex_defaults() -> AgentRunDefaults {
    let config =
        home_path(".codex/config.toml").and_then(|path| std::fs::read_to_string(path).ok());

    AgentRunDefaults {
        kind: AgentKind::Codex,
        model: config
            .as_deref()
            .and_then(|raw| simple_toml_string(raw, "model")),
        reasoning: config
            .as_deref()
            .and_then(|raw| simple_toml_string(raw, "model_reasoning_effort")),
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn simple_toml_string(raw: &str, key: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (left, right) = line.split_once('=')?;
        if left.trim() != key {
            return None;
        }
        let value = unquote_toml_string(right.trim())?;
        (!value.is_empty()).then_some(value)
    })
}

fn unquote_toml_string(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return value
            .split('#')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }
    let tail = chars.as_str();
    let end = tail.find(quote)?;
    Some(tail[..end].trim().to_string()).filter(|value| !value.is_empty())
}

fn home_path(relative: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(relative))
}

/// Map an adapter [`NormalizedEvent`] to the UI-facing [`SessionEvent`].
fn map_event(session_id: &str, task_id: &str, ev: &NormalizedEvent) -> SessionEvent {
    let (role, kind, text, data): (&str, &str, Option<String>, serde_json::Value) = match ev {
        NormalizedEvent::SessionStarted { session_id } => (
            "system",
            "session_started",
            None,
            json!({ "agent_session_id": session_id }),
        ),
        NormalizedEvent::AssistantText { text } => {
            ("assistant", "assistant_text", Some(text.clone()), json!({}))
        }
        NormalizedEvent::AssistantTextDelta { delta } => (
            "assistant",
            "assistant_text",
            Some(delta.clone()),
            json!({ "streaming": true }),
        ),
        NormalizedEvent::ToolUse { name, input } => (
            "tool",
            "tool_use",
            Some(name.clone()),
            json!({ "input": input }),
        ),
        NormalizedEvent::ToolResult { ok, summary } => (
            "tool",
            "tool_result",
            Some(compact_event_detail(summary)),
            json!({ "ok": ok, "summary": capped_event_detail(summary) }),
        ),
        NormalizedEvent::FileChanged { path, change } => (
            "app",
            "file_changed",
            Some(format!(
                "{} {}",
                change_label(*change),
                path.to_string_lossy()
            )),
            json!({ "change": change }),
        ),
        NormalizedEvent::TokenUsage { input, output } => (
            "system",
            "token_usage",
            Some(format!("Token usage: {input} in / {output} out")),
            json!({ "input": input, "output": output }),
        ),
        NormalizedEvent::AwaitingApproval { detail } => (
            "system",
            "awaiting_approval",
            Some(detail.clone()),
            json!({}),
        ),
        NormalizedEvent::UsageLimitReached { reset_at } => (
            "system",
            "usage_limit",
            Some("Usage limit reached".to_string()),
            json!({ "reset_at": reset_at }),
        ),
        NormalizedEvent::NetworkUnavailable { message } => (
            "system",
            "network_unavailable",
            Some(message.clone()),
            json!({}),
        ),
        NormalizedEvent::Error { message, retryable } => (
            "system",
            "error",
            Some(message.clone()),
            json!({ "retryable": retryable }),
        ),
        NormalizedEvent::SessionEnded { status } => (
            "system",
            "session_ended",
            Some(format!("{status:?}")),
            json!({ "status": status }),
        ),
    };

    SessionEvent {
        id: new_id(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        role: role.to_string(),
        kind: kind.to_string(),
        text,
        data,
        ts: now(),
    }
}

fn compact_event_detail(value: &str) -> String {
    let first_line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut compact = first_line.trim().to_string();
    if compact.is_empty() {
        compact = "Completed".to_string();
    }
    truncate_chars(&compact, 160)
}

fn capped_event_detail(value: &str) -> String {
    truncate_chars(value, MAX_EVENT_DETAIL_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value.chars().take(max_chars).collect::<String>();
    out.push_str("\n\n[details truncated]");
    out
}

fn change_label(change: am_agents::ChangeKind) -> &'static str {
    match change {
        am_agents::ChangeKind::Created => "Created",
        am_agents::ChangeKind::Modified => "Edited",
        am_agents::ChangeKind::Deleted => "Deleted",
    }
}

#[cfg(test)]
mod model_catalog_tests {
    use super::*;

    #[test]
    fn parses_codex_debug_models_without_large_payloads() {
        let raw = r#"{
          "models": [
            {
              "slug": "gpt-5.6-sol",
              "display_name": "GPT-5.6-Sol",
              "visibility": "list",
              "base_instructions": "huge text ignored",
              "default_reasoning_level": "medium",
              "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "high"}, {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}]
            },
            { "slug": "hidden-model", "visibility": "hidden" }
          ]
        }"#;
        let (models, reasoning) = parse_codex_debug_models(raw, Some("gpt-5.6-sol")).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert!(models[0].default);
        assert_eq!(models[0].default_reasoning.as_deref(), Some("medium"));
        assert_eq!(
            reasoning,
            vec!["low", "medium", "high", "xhigh", "max", "ultra"]
        );
    }

    #[test]
    fn parses_claude_aliases_and_full_ids_from_help() {
        let help =
            "Usage: claude --model fable|opus|sonnet\nExamples: claude --model claude-fable-5";
        let models = parse_claude_help_models(help);
        assert!(models.iter().any(|model| model.id == "fable"));
        assert!(models.iter().any(|model| model.id == "opus"));
        assert!(models.iter().any(|model| model.id == "sonnet"));
        assert!(models.iter().any(|model| model.id == "claude-fable-5"));
    }

    #[test]
    fn parses_codex_app_server_model_list() {
        let data = serde_json::json!([
            {
                "id": "gpt-5.3-codex",
                "model": "gpt-5.3-codex",
                "displayName": "gpt-5.3-codex",
                "hidden": false,
                "isDefault": true,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": ""},
                    {"reasoningEffort": "medium", "description": ""},
                    {"reasoningEffort": "high", "description": ""},
                    {"reasoningEffort": "xhigh", "description": ""}
                ]
            },
            {
                "id": "gpt-5.1-codex-mini",
                "model": "gpt-5.1-codex-mini",
                "displayName": "gpt-5.1-codex-mini",
                "hidden": false,
                "isDefault": false,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "medium", "description": ""},
                    {"reasoningEffort": "high", "description": ""}
                ]
            },
            { "id": "secret", "model": "secret", "hidden": true, "isDefault": false,
              "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [],
              "displayName": "secret", "description": "" }
        ]);
        let parsed: Vec<CodexAppServerModel> = serde_json::from_value(data).unwrap();
        let (models, reasoning) = collect_app_server_models(parsed, None);
        assert_eq!(models.len(), 2);
        assert!(models[0].default);
        assert_eq!(models[0].reasoning, vec!["low", "medium", "high", "xhigh"]);
        assert_eq!(models[0].default_reasoning.as_deref(), Some("medium"));
        assert_eq!(models[1].reasoning, vec!["medium", "high"]);
        assert_eq!(reasoning, vec!["low", "medium", "high", "xhigh"]);
    }

    #[test]
    fn app_server_default_yields_to_configured_model() {
        let data = serde_json::json!([
            { "id": "gpt-a", "model": "gpt-a", "displayName": "A", "hidden": false,
              "isDefault": true, "defaultReasoningEffort": "medium",
              "supportedReasoningEfforts": [] },
            { "id": "gpt-b", "model": "gpt-b", "displayName": "B", "hidden": false,
              "isDefault": false, "defaultReasoningEffort": "medium",
              "supportedReasoningEfforts": [] }
        ]);
        let parsed: Vec<CodexAppServerModel> = serde_json::from_value(data).unwrap();
        let (models, _) = collect_app_server_models(parsed, Some("gpt-b"));
        assert!(!models[0].default);
        assert!(models[1].default);
    }

    #[test]
    fn curated_claude_models_are_versioned_with_per_model_effort() {
        let models = curated_claude_models(None);
        let fable = models.iter().find(|m| m.id == "claude-fable-5").unwrap();
        assert_eq!(fable.label, "Claude Fable 5");
        assert!(fable.aliases.iter().any(|alias| alias == "fable"));
        assert_eq!(fable.reasoning, ["low", "medium", "high", "xhigh", "max"]);
        let opus = models.iter().find(|m| m.id == "claude-opus-4-8").unwrap();
        assert_eq!(opus.label, "Claude Opus 4.8");
        let opus46 = models.iter().find(|m| m.id == "claude-opus-4-6").unwrap();
        assert!(!opus46.reasoning.iter().any(|level| level == "xhigh"));
        let haiku = models.iter().find(|m| m.id == "claude-haiku-4-5").unwrap();
        assert!(haiku.reasoning.is_empty());
    }

    #[test]
    fn curated_claude_models_intersect_with_cli_levels() {
        let levels: Vec<String> = ["low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let models = curated_claude_models(Some(&levels));
        let fable = models.iter().find(|m| m.id == "claude-fable-5").unwrap();
        assert_eq!(fable.reasoning, ["low", "medium", "high"]);
    }

    #[test]
    fn help_aliases_fold_into_curated_versioned_entries() {
        let mut models = curated_claude_models(None);
        let before = models.len();
        for option in parse_claude_help_models(
            "Provide an alias for the latest model (e.g. 'fable', 'opus', or 'sonnet')",
        ) {
            push_model_option(&mut models, option);
        }
        assert_eq!(models.len(), before, "aliases must not create duplicates");
    }

    #[test]
    fn parses_effort_levels_from_help_parenthetical() {
        let help = "  --effort <level>   Effort level for the current session\n\
                    (low, medium, high, xhigh, max)";
        assert_eq!(
            parse_claude_effort_levels(help).unwrap(),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            parse_claude_effort_levels("--effort <level> (low, medium, high, ultra)").unwrap(),
            ["low", "medium", "high", "ultra"]
        );
        assert!(parse_claude_effort_levels("no efforts here at all").is_none());
    }

    /// Live check against the locally installed Codex CLI. Ignored by default
    /// so CI stays hermetic; run with `cargo test -p am-core -- --ignored`.
    #[test]
    #[ignore]
    fn live_codex_app_server_model_list() {
        let Some(binary) = find_binary("codex") else {
            eprintln!("codex not installed; skipping");
            return;
        };
        let (models, reasoning) = codex_app_server_models(&binary, None).unwrap();
        assert!(!models.is_empty(), "expected at least one model");
        assert!(!reasoning.is_empty(), "expected at least one effort level");
        for model in &models {
            eprintln!(
                "{} ({}) efforts={:?} default_effort={:?} default={}",
                model.label, model.id, model.reasoning, model.default_reasoning, model.default
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_output_timeout_drains_large_catalogs_while_process_runs() {
        let output = command_output_timeout(
            Path::new("/bin/sh"),
            &["-c", "yes x | head -c 300000"],
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.len(), 300_000);
    }

    #[test]
    fn compact_event_detail_caps_tool_output() {
        let long = "first line\n".to_string() + &"x".repeat(MAX_EVENT_DETAIL_CHARS + 50);
        assert_eq!(compact_event_detail(&long), "first line");
        assert!(capped_event_detail(&long).contains("[details truncated]"));
    }
}

#[cfg(test)]
mod repo_path_tests {
    use super::*;

    #[test]
    fn matches_git_toplevel_against_platform_paths() {
        assert!(same_local_path("C:/Users/dev/app", "C:/Users/dev/app/"));
        assert!(same_local_path("/home/dev/app", "/home/dev/app"));
        assert!(!same_local_path("/home/dev/app", "/home/dev/other"));
    }

    #[cfg(windows)]
    #[test]
    fn ignores_separator_and_case_differences_on_windows() {
        assert!(same_local_path("C:/Users/dev/app", r"c:\users\dev\app"));
        assert!(same_local_path(r"\\?\C:\Users\dev\app", "C:/Users/dev/app"));
    }
}

#[cfg(test)]
mod repo_connection_tests {
    use super::*;
    use crate::test_core;

    /// The checked-out working copy is a real git repo with history, which is
    /// what `connect_local_repo` validates against.
    fn workspace_root() -> String {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_string_lossy()
            .to_string()
    }

    #[tokio::test]
    async fn connecting_the_same_path_twice_reuses_the_existing_repo() {
        let core = test_core().await;
        let project = core.ensure_workbench_project().await.unwrap();
        let path = workspace_root();

        let first = core
            .connect_local_repo(NewLocalRepo {
                project_id: project.id.clone(),
                path: path.clone(),
            })
            .await
            .unwrap();
        let second = core
            .connect_local_repo(NewLocalRepo {
                project_id: project.id.clone(),
                path,
            })
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(core.list_repos(&project.id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disconnecting_removes_the_repo() {
        let core = test_core().await;
        let project = core.ensure_workbench_project().await.unwrap();
        let repo = core
            .connect_local_repo(NewLocalRepo {
                project_id: project.id.clone(),
                path: workspace_root(),
            })
            .await
            .unwrap();

        core.delete_repo(&repo.id).await.unwrap();
        assert!(core.list_repos(&project.id).await.unwrap().is_empty());
        assert!(core.delete_repo(&repo.id).await.is_err());
    }

    #[tokio::test]
    async fn clearing_removes_every_repo_in_the_project() {
        let core = test_core().await;
        let project = core.ensure_workbench_project().await.unwrap();
        core.connect_local_repo(NewLocalRepo {
            project_id: project.id.clone(),
            path: workspace_root(),
        })
        .await
        .unwrap();

        let removed = core.clear_project_repos(&project.id).await.unwrap();
        assert_eq!(removed, 1);
        assert!(core.list_repos(&project.id).await.unwrap().is_empty());
    }
}
