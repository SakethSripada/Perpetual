use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use am_agents::{
    AgentKind, NormalizedEvent, PermissionPolicy, QuotaWindowKind, SessionHandle, SessionRef,
    SessionSpec, SessionStatus,
};
use am_proto::{
    new_id, now, AgentThread, AgentThreadApplyResult, AgentThreadDiff, AgentThreadEvent,
    AgentThreadRepoApplyResult, AgentThreadRepoDiff, AgentThreadUpdate, AppEvent,
    AvailabilityState, ExecutionBackend, FileChange, ModelTargetKind, NewAgentThread,
    NewWorkbenchSessionGroup, Project, QueuedTurn, SessionState, TaskBudget, TaskDiff, TaskStatus,
    WorkbenchSessionGroup, WorkbenchSessionGroupUpdate,
};
use serde_json::json;
use tokio::sync::mpsc::Receiver;

use crate::local_models::{
    legacy_run_target_hash, normalize_model_target, run_target_hash, target_hash_matches,
};
use crate::policy::PolicyPreflightInput;
use crate::sandbox::SandboxLease;
use crate::{AppCore, ApprovalScope, CoreError};

const THREAD_CONTEXT_FILE: &str = "TASK_CONTEXT.md";
const CLAUDE_FILE: &str = "CLAUDE.md";
const AGENTS_FILE: &str = "AGENTS.md";
const GENERATED_CONTEXT_FILES: &[&str] = &[THREAD_CONTEXT_FILE, CLAUDE_FILE, AGENTS_FILE];
const MAX_THREAD_PROGRESS_BYTES: usize = 20 * 1024;
const MAX_EVENT_TEXT_CHARS: usize = 2_000;
const CANCEL_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone)]
struct PendingThreadMessage {
    text: String,
    echo_user_message: bool,
    client_message_id: Option<String>,
}

impl PendingThreadMessage {
    fn public(text: String, client_message_id: Option<String>) -> Self {
        Self {
            text,
            echo_user_message: true,
            client_message_id,
        }
    }

    fn from_queued(queued: QueuedTurn) -> Self {
        Self {
            text: queued.message,
            echo_user_message: queued.echo_user_message,
            client_message_id: queued.client_message_id,
        }
    }
}

#[derive(Debug, Clone)]
struct ThreadWorkspace {
    path: PathBuf,
    uses_visible_repo: bool,
}

impl AppCore {
    pub async fn ensure_workbench_project(&self) -> Result<Project, CoreError> {
        if let Some(existing) = self
            .list_projects()
            .await?
            .into_iter()
            .find(|project| project.name == "Workbench")
        {
            return Ok(existing);
        }

        self.create_project(am_proto::NewProject {
            name: "Workbench".to_string(),
            description: Some("Default workspace for agent sessions.".to_string()),
        })
        .await
    }

    pub async fn list_agent_threads(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<AgentThread>, CoreError> {
        Ok(am_db::repos::agent_thread::list(&self.db.pool, project_id).await?)
    }

    pub async fn list_workbench_session_groups(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<WorkbenchSessionGroup>, CoreError> {
        Ok(am_db::repos::agent_thread::list_groups(&self.db.pool, project_id).await?)
    }

    pub async fn create_workbench_session_group(
        &self,
        input: NewWorkbenchSessionGroup,
    ) -> Result<WorkbenchSessionGroup, CoreError> {
        let group = am_db::repos::agent_thread::create_group(&self.db.pool, input).await?;
        self.activity(
            group.project_id.clone(),
            None,
            "thread_group.created",
            json!({ "group_id": group.id, "name": group.name }),
        )
        .await?;
        Ok(group)
    }

    pub async fn update_workbench_session_group(
        &self,
        id: &str,
        patch: WorkbenchSessionGroupUpdate,
    ) -> Result<WorkbenchSessionGroup, CoreError> {
        let group = am_db::repos::agent_thread::update_group(&self.db.pool, id, patch).await?;
        self.activity(
            group.project_id.clone(),
            None,
            "thread_group.updated",
            json!({ "group_id": group.id, "name": group.name }),
        )
        .await?;
        Ok(group)
    }

    pub async fn delete_workbench_session_group(&self, id: &str) -> Result<(), CoreError> {
        am_db::repos::agent_thread::delete_group(&self.db.pool, id).await?;
        self.activity(
            None,
            None,
            "thread_group.deleted",
            json!({ "group_id": id }),
        )
        .await?;
        Ok(())
    }

    pub async fn assign_agent_thread_group(
        &self,
        thread_id: &str,
        group_id: Option<&str>,
    ) -> Result<AgentThread, CoreError> {
        let thread =
            am_db::repos::agent_thread::assign_group(&self.db.pool, thread_id, group_id).await?;
        self.events
            .publish(AppEvent::AgentThreadUpdated(thread.clone()));
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.group_assigned",
            json!({ "thread_id": thread.id, "group_id": thread.group_id.clone() }),
        )
        .await?;
        Ok(thread)
    }

    pub async fn create_agent_thread(
        &self,
        mut input: NewAgentThread,
    ) -> Result<AgentThread, CoreError> {
        if input.execution_backend.is_none() {
            input.execution_backend = Some(
                self.get_sandbox_policy()
                    .await
                    .unwrap_or_default()
                    .default_backend,
            );
        }
        let repo_ids = input.repo_ids.clone();
        for repo_id in &repo_ids {
            am_db::repos::repo::get(&self.db.pool, repo_id)
                .await?
                .ok_or(CoreError::NotFound)?;
        }

        let thread = am_db::repos::agent_thread::create(&self.db.pool, input).await?;
        if !repo_ids.is_empty() {
            am_db::repos::agent_thread_repo::replace_repos(&self.db.pool, &thread.id, &repo_ids)
                .await?;
        }
        self.events
            .publish(AppEvent::AgentThreadCreated(thread.clone()));
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.created",
            json!({ "thread_id": thread.id, "title": thread.title }),
        )
        .await?;
        Ok(thread)
    }

    pub async fn get_agent_thread(&self, id: &str) -> Result<Option<AgentThread>, CoreError> {
        Ok(am_db::repos::agent_thread::get(&self.db.pool, id).await?)
    }

    pub async fn update_agent_thread(
        &self,
        id: &str,
        patch: AgentThreadUpdate,
    ) -> Result<AgentThread, CoreError> {
        if let Some(requested) = patch.task_budget.as_ref() {
            let current = am_db::repos::agent_thread::get(&self.db.pool, id)
                .await?
                .ok_or(CoreError::NotFound)?;
            if self.sessions.is_active(id).await && current.task_budget != *requested {
                return Err(CoreError::Other(
                    "Stop the session before changing its task budget.".into(),
                ));
            }
            let has_started = !am_db::repos::agent_turn::list_for_thread(&self.db.pool, id)
                .await?
                .is_empty();
            crate::budget::validate_change(&current.task_budget, requested, has_started)
                .map_err(CoreError::Other)?;
        }
        let thread = am_db::repos::agent_thread::update(&self.db.pool, id, patch).await?;
        self.events
            .publish(AppEvent::AgentThreadUpdated(thread.clone()));
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.updated",
            json!({ "thread_id": thread.id, "status": thread.status.as_str() }),
        )
        .await?;
        if matches!(thread.status, TaskStatus::Done | TaskStatus::Cancelled)
            && !self.sessions.is_active(&thread.id).await
        {
            self.cleanup_thread_sandboxes(&thread.id).await;
        }
        Ok(thread)
    }

    pub async fn delete_agent_thread(&self, id: &str, force: bool) -> Result<(), CoreError> {
        let active = self.sessions.is_active(id).await;
        if active && !force {
            return Err(CoreError::Other(
                "Stop the running session before deleting it.".into(),
            ));
        }
        let thread = am_db::repos::agent_thread::get(&self.db.pool, id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if active {
            let _ = self.sessions.cancel(id).await;
            if !self.sessions.wait_until_inactive(id, CANCEL_SETTLE).await {
                return Err(CoreError::Other(
                    "the running session did not stop before deletion timed out".into(),
                ));
            }
        }
        self.cleanup_thread_sandboxes(id).await;
        am_db::repos::agent_thread::delete(&self.db.pool, id).await?;
        self.activity(
            thread.project_id,
            None,
            if active {
                "thread.force_deleted"
            } else {
                "thread.deleted"
            },
            json!({ "thread_id": id, "was_running": active }),
        )
        .await?;
        Ok(())
    }

    pub async fn assign_thread_repos(
        &self,
        thread_id: &str,
        repo_ids: Vec<String>,
    ) -> Result<Vec<am_proto::AgentThreadRepo>, CoreError> {
        let thread = am_db::repos::agent_thread::get(&self.db.pool, thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        for repo_id in &repo_ids {
            am_db::repos::repo::get(&self.db.pool, repo_id)
                .await?
                .ok_or(CoreError::NotFound)?;
        }
        let existing =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, thread_id).await?;
        if existing.iter().any(|repo| repo.worktree_path.is_some()) {
            return Err(CoreError::Other(
                "cannot change repositories after a thread workspace has been created".into(),
            ));
        }
        am_db::repos::agent_thread_repo::replace_repos(&self.db.pool, thread_id, &repo_ids).await?;
        self.activity(
            thread.project_id,
            None,
            "thread.repos_selected",
            json!({ "thread_id": thread_id, "repo_count": repo_ids.len() }),
        )
        .await?;
        self.list_thread_repos(thread_id).await
    }

    pub async fn list_thread_repos(
        &self,
        thread_id: &str,
    ) -> Result<Vec<am_proto::AgentThreadRepo>, CoreError> {
        Ok(am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, thread_id).await?)
    }

    pub async fn run_agent_thread(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
    ) -> Result<String, CoreError> {
        self.run_agent_thread_inner(
            thread_id,
            agent,
            permission,
            message.map(|message| PendingThreadMessage::public(message, None)),
            None,
        )
        .await
    }

    pub async fn run_agent_thread_with_client_message(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
        execution_backend: Option<ExecutionBackend>,
        client_message_id: Option<String>,
    ) -> Result<String, CoreError> {
        self.run_agent_thread_inner(
            thread_id,
            agent,
            permission,
            message.map(|message| PendingThreadMessage::public(message, client_message_id)),
            execution_backend,
        )
        .await
    }

    pub async fn run_agent_thread_with_backend(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, CoreError> {
        self.run_agent_thread_inner(
            thread_id,
            agent,
            permission,
            message.map(|message| PendingThreadMessage::public(message, None)),
            execution_backend,
        )
        .await
    }

    async fn run_agent_thread_inner(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<PendingThreadMessage>,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, CoreError> {
        if self.sessions.is_active(thread_id).await {
            return Err(CoreError::Other("agent thread is already running".into()));
        }
        if am_db::repos::cloud_run::active_for_thread(&self.db.pool, thread_id)
            .await?
            .is_some()
        {
            return Err(CoreError::Other(
                "this thread is running in the cloud; reclaim the cloud run before starting a local turn".into(),
            ));
        }
        let mut thread = am_db::repos::agent_thread::get(&self.db.pool, thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let requested_backend = self
            .thread_execution_backend(&thread, execution_backend)
            .await?;
        let model_target = normalize_model_target(thread.model_target, thread.local_provider);
        let policy = self
            .policy_preflight(PolicyPreflightInput {
                agent,
                model: thread.model.clone(),
                runtime: requested_backend,
            })
            .await?;
        let agent = policy.agent;
        let backend = policy.runtime;
        thread.model = policy.model.clone().or_else(|| thread.model.clone());
        thread.model_target = model_target;
        thread.task_budget.validate().map_err(CoreError::Other)?;

        let adapter = self.agents.get(agent).ok_or_else(|| {
            CoreError::Other(format!("no adapter available for {}", agent.label()))
        })?;
        let local_model = if model_target == ModelTargetKind::RentedCompute {
            return Err(CoreError::Other(
                "rented compute targets are not supported by the VS Code extension".into(),
            ));
        } else {
            self.local_model_runtime(
                thread.local_provider,
                thread.model.clone(),
                thread.local_base_url.clone(),
            )?
        };
        if local_model.is_some() && agent != AgentKind::Codex {
            return Err(CoreError::Other(
                "open-model runs use Codex OSS in this version".into(),
            ));
        }
        if backend == ExecutionBackend::DockerSandbox
            && local_model
                .as_ref()
                .is_some_and(local_model_uses_container_localhost)
        {
            return Err(CoreError::Other(
                "local model endpoints on localhost are not reachable from Docker Sandbox. Use Host execution, or set the local endpoint to a host-reachable address such as http://host.docker.internal:<port>."
                .into(),
            ));
        }
        validate_runtime_budget(&thread.task_budget, agent, backend, local_model.is_some())?;
        if local_model.is_none() {
            if let Some(reset_at) = self.known_limited_agent_reset(agent).await? {
                if crate::budget::is_percentage_budget(&thread.task_budget) {
                    thread.status = TaskStatus::Paused;
                    let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                    self.events.publish(AppEvent::AgentThreadUpdated(saved));
                    return Err(CoreError::Other(
                        "Percentage budgets pause when the selected provider is limited; they do not switch providers because quota percentages are not comparable.".into(),
                    ));
                }
                return self
                    .start_known_limited_thread_fallback(
                        thread,
                        agent,
                        permission,
                        message,
                        reset_at,
                        policy.envelope_id.clone(),
                    )
                    .await;
            }
        }
        if let TaskBudget::Tokens { limit_tokens } = &thread.task_budget {
            let consumed = am_db::repos::usage_ledger::total_for_session(&self.db.pool, thread_id)
                .await
                .unwrap_or(0);
            if consumed >= *limit_tokens {
                thread.status = TaskStatus::Paused;
                let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events.publish(AppEvent::AgentThreadUpdated(saved));
                return Err(CoreError::Other(
                    "This task budget is exhausted. Increase the cap or turn budgeting off before resuming.".into(),
                ));
            }
        }
        let target_hash = run_target_hash(
            agent,
            thread.model.as_deref(),
            thread.reasoning.as_deref(),
            thread.local_provider,
            thread.local_base_url.as_deref(),
            backend,
            model_target,
            thread.compute_lease_id.as_deref(),
        );
        let legacy_target_hash = legacy_run_target_hash(
            agent,
            thread.model.as_deref(),
            thread.reasoning.as_deref(),
            thread.local_provider,
            thread.local_base_url.as_deref(),
        );
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
        let workspace = self
            .ensure_thread_workspace(&thread, backend, permission)
            .await?;
        self.render_thread_context_files(&thread, &workspace.path)
            .await?;
        let prior = self
            .latest_thread_session_ref(thread_id, agent, &target_hash, &legacy_target_hash)
            .await?;
        let resumed_agent_session_id = prior.as_ref().map(|p| p.agent_session_id.clone());
        // A resume/fallback turn is started with no message (`None`). If the user
        // sent input that is still pending (e.g. their question was carried over
        // from a turn that hit a usage limit, or queued while the agent ran),
        // deliver it now instead of resuming with a generic "continue" prompt —
        // otherwise the agent never answers what was actually asked.
        let mut message = match message {
            Some(msg) => Some(msg),
            None => am_db::repos::queued_turn::pop_next(&self.db.pool, thread_id)
                .await?
                .map(PendingThreadMessage::from_queued),
        };
        let permission_string = permission_to_string(permission);
        self.sync_session_capacity().await;
        let permit = match self.sessions.try_acquire(thread.project_id.as_deref()) {
            Ok(permit) => permit,
            Err(_) => {
                let queued_turn_id = match message
                    .as_ref()
                    .map(|msg| (msg.text.trim(), msg.echo_user_message))
                    .filter(|(msg, _)| !msg.is_empty())
                {
                    Some((msg, echo_user_message)) => {
                        let queued = am_db::repos::queued_turn::enqueue_with_echo(
                            &self.db.pool,
                            thread_id,
                            agent,
                            &permission_string,
                            msg,
                            policy.envelope_id.as_deref(),
                            echo_user_message,
                            message
                                .as_ref()
                                .and_then(|msg| msg.client_message_id.as_deref()),
                        )
                        .await?;
                        message = None;
                        Some(queued.id)
                    }
                    None => None,
                };
                thread.status = TaskStatus::Queued;
                let queued = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events
                    .publish(AppEvent::AgentThreadUpdated(queued.clone()));
                self.activity(
                    queued.project_id.clone(),
                    None,
                    "thread.capacity_queued",
                    json!({
                        "thread_id": thread_id,
                        "agent": agent.as_str(),
                        "queue_id": queued_turn_id,
                    }),
                )
                .await?;
                self.sessions
                    .acquire_queued(thread.project_id.as_deref())
                    .await
            }
        };
        if message.is_none() {
            message = am_db::repos::queued_turn::pop_next(&self.db.pool, thread_id)
                .await?
                .map(PendingThreadMessage::from_queued);
        }
        let sandbox_name = (backend == ExecutionBackend::DockerSandbox)
            .then(|| Self::sandbox_name_for("thread", thread_id));
        let (runtime, sandbox_lease) = self
            .session_runtime(agent, backend, sandbox_name.clone())
            .await?;
        let turn = am_db::repos::agent_turn::create(
            &self.db.pool,
            thread_id,
            agent,
            &permission_string,
            backend,
            sandbox_name.as_deref(),
            thread.model.as_deref(),
            thread.reasoning.as_deref(),
            thread.local_provider,
            thread.local_base_url.as_deref(),
            model_target,
            thread.compute_lease_id.as_deref(),
            thread.compute_provider,
            thread.estimated_compute_cost_usd,
            thread.fallback_model_target,
            Some(&target_hash),
            policy.envelope_id.as_deref(),
        )
        .await?;
        let user_message = match (&message, prior.is_some()) {
            (Some(msg), _) if msg.echo_user_message => Some(msg.text.trim()),
            (Some(_), _) => None,
            (None, false) => Some(thread.objective.trim()),
            (None, true) => None,
        }
        .filter(|msg| !msg.is_empty());
        let user_client_message_id = match &message {
            Some(msg) if msg.echo_user_message => msg.client_message_id.as_deref(),
            _ => None,
        };

        if let Some(message) = user_message {
            let ev = user_thread_event(thread_id, &turn.id, message, user_client_message_id);
            am_db::repos::agent_thread_message::insert(&self.db.pool, &ev).await?;
            self.events.publish(AppEvent::AgentThreadEvent(ev));
        }

        thread.status = TaskStatus::Running;
        thread.active_agent = Some(agent);
        thread.permission = permission_string.clone();
        thread.execution_backend = backend;
        if thread.preferred_agent.is_none() {
            thread.preferred_agent = Some(agent);
        }
        let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
        self.events
            .publish(AppEvent::AgentThreadUpdated(thread.clone()));
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.turn_started",
            json!({
                "thread_id": thread_id,
                "turn_id": turn.id,
                "agent": agent.as_str(),
                "resumed_agent_session_id": resumed_agent_session_id,
            }),
        )
        .await?;

        let context_files_available = !workspace.uses_visible_repo;
        let workspace_path = workspace.path;
        let mut runtime_policy = policy.runtime_policy.clone();
        runtime_policy.task_budget = Some(thread.task_budget.clone());
        let spec = SessionSpec {
            worktree: workspace_path,
            prompt: match (&message, prior.is_some()) {
                (Some(msg), true) => build_thread_followup_prompt(&thread, &msg.text),
                (Some(msg), false) => {
                    build_thread_initial_prompt(&thread, &msg.text, context_files_available)
                }
                (None, true) => build_thread_resume_prompt(&thread, context_files_available),
                (None, false) => {
                    build_thread_initial_prompt(&thread, &thread.objective, context_files_available)
                }
            },
            model: thread.model.clone(),
            reasoning: thread.reasoning.clone(),
            local_model,
            permission,
            runtime,
            policy: Some(runtime_policy),
            approver: self.approver_for(
                permission,
                agent,
                ApprovalScope {
                    project_id: thread.project_id.clone(),
                    thread_id: Some(thread_id.to_string()),
                    session_id: Some(turn.id.clone()),
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

        self.sessions.register(thread_id, control).await;

        let core = self.clone();
        let tid = thread_id.to_string();
        let turn_id = turn.id.clone();
        tokio::spawn(async move {
            core.consume_agent_thread_turn(
                turn_id,
                tid,
                agent,
                events,
                permit,
                sandbox_lease,
                message,
            )
            .await;
        });

        Ok(turn.id)
    }

    pub async fn stop_agent_thread(&self, thread_id: &str) -> Result<(), CoreError> {
        if !self.sessions.cancel(thread_id).await {
            return Err(CoreError::Other("agent thread is not running".into()));
        }
        Ok(())
    }

    pub async fn send_thread_message(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
        client_message_id: Option<String>,
    ) -> Result<Option<String>, CoreError> {
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err(CoreError::Other("message is empty".into()));
        }
        let permission_string = permission_to_string(permission);
        // A cloud-active thread has no local session, but starting one would
        // race the provider run. Queue instead; the reclaim path drains the
        // queue as soon as work returns from the cloud.
        let cloud_active = am_db::repos::cloud_run::active_for_thread(&self.db.pool, thread_id)
            .await?
            .is_some();
        if self.sessions.is_active(thread_id).await || cloud_active {
            let queued = am_db::repos::queued_turn::enqueue_with_echo(
                &self.db.pool,
                thread_id,
                agent,
                &permission_string,
                &message,
                None,
                true,
                client_message_id.as_deref(),
            )
            .await?;
            let thread = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await?;
            self.activity(
                thread.and_then(|t| t.project_id),
                None,
                "thread.message_queued",
                json!({ "thread_id": thread_id, "queue_id": queued.id }),
            )
            .await?;
            Ok(None)
        } else {
            self.run_agent_thread_inner(
                thread_id,
                agent,
                permission,
                Some(PendingThreadMessage::public(message, client_message_id)),
                None,
            )
            .await
            .map(Some)
        }
    }

    pub async fn list_thread_events(
        &self,
        thread_id: &str,
    ) -> Result<Vec<AgentThreadEvent>, CoreError> {
        Ok(am_db::repos::agent_thread_message::list_for_thread(&self.db.pool, thread_id).await?)
    }

    pub async fn list_thread_turns(
        &self,
        thread_id: &str,
    ) -> Result<Vec<am_proto::AgentTurn>, CoreError> {
        Ok(am_db::repos::agent_turn::list_for_thread(&self.db.pool, thread_id).await?)
    }

    pub async fn list_queued_turns(&self, thread_id: &str) -> Result<Vec<QueuedTurn>, CoreError> {
        Ok(am_db::repos::queued_turn::list_for_thread(&self.db.pool, thread_id).await?)
    }

    pub async fn delete_queued_turn(&self, id: &str) -> Result<(), CoreError> {
        am_db::repos::queued_turn::delete(&self.db.pool, id).await?;
        Ok(())
    }

    pub async fn update_queued_turn(&self, id: &str, message: &str) -> Result<(), CoreError> {
        am_db::repos::queued_turn::update_message(&self.db.pool, id, message).await?;
        Ok(())
    }

    pub async fn reorder_queued_turns(
        &self,
        thread_id: &str,
        ordered_ids: Vec<String>,
    ) -> Result<(), CoreError> {
        am_db::repos::queued_turn::reorder(&self.db.pool, thread_id, &ordered_ids).await?;
        Ok(())
    }

    pub async fn thread_diff(&self, thread_id: &str) -> Result<AgentThreadDiff, CoreError> {
        let links =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, thread_id).await?;
        let mut repos = Vec::new();
        for link in links {
            let Some(worktree) = link.worktree_path.clone() else {
                continue;
            };
            let Some(base_ref) = link.base_ref.clone() else {
                continue;
            };
            let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id).await?;
            let uses_visible_repo = repo
                .as_ref()
                .and_then(|repo| repo.local_path.as_deref())
                .is_some_and(|path| same_path(Path::new(&worktree), Path::new(path)));
            let base_for_diff = base_ref.clone();
            let worktree_for_diff = worktree.clone();
            let mut diff = tokio::task::spawn_blocking(move || {
                if uses_visible_repo {
                    direct_repo_diff_with_excludes(
                        Path::new(&worktree_for_diff),
                        &base_ref,
                        am_vcs::MAX_DIFF_BYTES,
                        GENERATED_CONTEXT_FILES,
                    )
                } else {
                    am_vcs::worktree_diff_with_excludes(
                        Path::new(&worktree_for_diff),
                        &base_ref,
                        am_vcs::MAX_DIFF_BYTES,
                        GENERATED_CONTEXT_FILES,
                    )
                    .map_err(|err| CoreError::Other(err.to_string()))
                }
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))??;
            let remote_url = repo.as_ref().and_then(|repo| repo.remote_url.clone());
            repos.push(AgentThreadRepoDiff {
                repo_id: link.repo_id,
                repo_name: link.repo_name,
                remote_url,
                branch: diff.branch.take(),
                base_ref: Some(base_for_diff),
                head_ref: diff.head_ref.take(),
                worktree_path: diff.worktree_path.take(),
                files: diff.files,
                patch: diff.patch,
            });
        }
        Ok(AgentThreadDiff { repos })
    }

    pub async fn apply_thread_changes(
        &self,
        thread_id: &str,
    ) -> Result<AgentThreadApplyResult, CoreError> {
        am_db::repos::agent_thread::get(&self.db.pool, thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if self.sessions.is_active(thread_id).await {
            return Ok(AgentThreadApplyResult {
                thread_id: thread_id.to_string(),
                applied: false,
                repos: Vec::new(),
                blockers: vec![
                    "Stop the running session before applying managed changes.".to_string()
                ],
            });
        }

        struct PreparedRepo {
            result: AgentThreadRepoApplyResult,
            target: PathBuf,
            worktree: PathBuf,
            branch: Option<String>,
            workspace_backend: ExecutionBackend,
            patch: String,
        }

        let links =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, thread_id).await?;
        let mut prepared = Vec::new();
        let mut repos = Vec::new();
        let mut blockers = Vec::new();

        for link in links {
            let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id).await?;
            let target_path = repo.as_ref().and_then(|repo| repo.local_path.clone());
            let mut result = AgentThreadRepoApplyResult {
                repo_id: link.repo_id.clone(),
                repo_name: link.repo_name.clone(),
                target_path: target_path.clone(),
                worktree_path: link.worktree_path.clone(),
                files: Vec::new(),
                applied: false,
                blocker: None,
            };

            let Some(target_path) = target_path else {
                result.blocker = Some("Repository has no visible local path.".to_string());
                blockers.push(format!(
                    "{}: repository has no visible local path",
                    link.repo_name
                ));
                repos.push(result);
                continue;
            };
            let Some(worktree_path) = link.worktree_path.clone() else {
                repos.push(result);
                continue;
            };
            let Some(base_ref) = link.base_ref.clone() else {
                result.blocker = Some("Managed worktree has no base revision.".to_string());
                blockers.push(format!(
                    "{}: managed worktree has no base revision",
                    link.repo_name
                ));
                repos.push(result);
                continue;
            };

            let worktree = PathBuf::from(worktree_path);
            let target = PathBuf::from(target_path);
            let uses_visible_repo = same_path(&worktree, &target);
            let worktree_for_diff = worktree.clone();
            let base_for_diff = base_ref.clone();
            let diff = tokio::task::spawn_blocking(move || {
                if uses_visible_repo {
                    direct_repo_diff_with_excludes(
                        &worktree_for_diff,
                        &base_for_diff,
                        am_vcs::MAX_DIFF_BYTES,
                        GENERATED_CONTEXT_FILES,
                    )
                } else {
                    am_vcs::worktree_diff_with_excludes(
                        &worktree_for_diff,
                        &base_for_diff,
                        am_vcs::MAX_DIFF_BYTES,
                        GENERATED_CONTEXT_FILES,
                    )
                    .map_err(|err| CoreError::Other(err.to_string()))
                }
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))??;
            result.files = diff.files.clone();
            if result.files.is_empty() {
                repos.push(result);
                continue;
            }
            if uses_visible_repo {
                result.applied = true;
                repos.push(result);
                continue;
            }

            let paths = result
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            let target_for_dirty = target.clone();
            let dirty =
                tokio::task::spawn_blocking(move || am_vcs::dirty_paths(&target_for_dirty, &paths))
                    .await
                    .map_err(|e| CoreError::Other(e.to_string()))?
                    .map_err(|e| CoreError::Other(e.to_string()))?;
            if !dirty.is_empty() {
                let preview = dirty.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                let suffix = if dirty.len() > 5 { ", ..." } else { "" };
                let message = format!("Visible repo has uncommitted changes in {preview}{suffix}");
                result.blocker = Some(message.clone());
                blockers.push(format!("{}: {message}", link.repo_name));
                repos.push(result);
                continue;
            }

            let worktree_for_patch = worktree.clone();
            let base_for_patch = base_ref;
            let patch = tokio::task::spawn_blocking(move || {
                am_vcs::worktree_patch_with_excludes(
                    &worktree_for_patch,
                    &base_for_patch,
                    GENERATED_CONTEXT_FILES,
                )
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?
            .map_err(|e| CoreError::Other(e.to_string()))?;
            let target_for_check = target.clone();
            let patch_for_check = patch.clone();
            if let Err(err) = tokio::task::spawn_blocking(move || {
                am_vcs::check_patch_applies(&target_for_check, &patch_for_check)
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?
            {
                let message = format!("Patch does not apply cleanly: {err}");
                result.blocker = Some(message.clone());
                blockers.push(format!("{}: {message}", link.repo_name));
                repos.push(result);
                continue;
            }

            prepared.push(PreparedRepo {
                result,
                target,
                worktree,
                branch: link.branch.clone(),
                workspace_backend: link.workspace_backend,
                patch,
            });
        }

        if !blockers.is_empty() {
            repos.extend(prepared.into_iter().map(|item| item.result));
            return Ok(AgentThreadApplyResult {
                thread_id: thread_id.to_string(),
                applied: false,
                repos,
                blockers,
            });
        }

        for item in prepared {
            let target = item.target.clone();
            let patch = item.patch.clone();
            tokio::task::spawn_blocking(move || am_vcs::apply_patch_to_repo(&target, &patch))
                .await
                .map_err(|e| CoreError::Other(e.to_string()))?
                .map_err(|e| CoreError::Other(e.to_string()))?;
            let cleanup_target = item.target.clone();
            let cleanup_worktree = item.worktree.clone();
            let cleanup_branch = item.branch.clone();
            let cleanup_backend = item.workspace_backend;
            let cleanup = tokio::task::spawn_blocking(move || {
                cleanup_managed_workspace(
                    &cleanup_target,
                    &cleanup_worktree,
                    cleanup_branch.as_deref(),
                    cleanup_backend,
                )
            })
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;
            let mut result = item.result;
            result.applied = true;
            if cleanup.is_ok() {
                am_db::repos::agent_thread_repo::upsert(
                    &self.db.pool,
                    thread_id,
                    &result.repo_id,
                    None,
                    None,
                    None,
                    item.workspace_backend,
                )
                .await?;
            } else if let Err(err) = cleanup {
                self.activity(
                    None,
                    None,
                    "thread.workspace_cleanup_failed",
                    json!({
                        "thread_id": thread_id,
                        "repo_id": result.repo_id,
                        "worktree_path": item.worktree.to_string_lossy(),
                        "reason": err.to_string(),
                    }),
                )
                .await?;
            }
            repos.push(result);
        }

        let applied = repos.iter().any(|repo| repo.applied);
        self.activity(
            None,
            None,
            "thread.changes_applied",
            json!({
                "thread_id": thread_id,
                "repo_count": repos.iter().filter(|repo| repo.applied).count(),
            }),
        )
        .await?;
        Ok(AgentThreadApplyResult {
            thread_id: thread_id.to_string(),
            applied,
            repos,
            blockers,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn consume_agent_thread_turn(
        &self,
        turn_id: String,
        thread_id: String,
        agent: AgentKind,
        mut events: Receiver<NormalizedEvent>,
        permit: crate::SessionPermit,
        sandbox_lease: Option<SandboxLease>,
        pending_message: Option<PendingThreadMessage>,
    ) {
        let mut saw_usage_limit = false;
        let mut saw_network_loss = false;
        let mut saw_approval_needed = false;
        let mut completed_ok = false;
        let mut limit_reset_at = None;
        let mut budget_exhausted = false;
        let mut saw_token_telemetry = false;
        let mut saw_five_hour_telemetry = false;
        let mut saw_weekly_telemetry = false;
        let mut usage_reconciler = crate::budget::UsageReconciler::default();
        let mut streaming_assistant: Option<AgentThreadEvent> = None;
        let usage_turn = am_db::repos::agent_turn::get(&self.db.pool, &turn_id)
            .await
            .ok()
            .flatten();
        let usage_thread = am_db::repos::agent_thread::get(&self.db.pool, &thread_id)
            .await
            .ok()
            .flatten();
        let usage_project_id = usage_thread
            .as_ref()
            .and_then(|thread| thread.project_id.clone());
        let usage_budget = usage_thread
            .as_ref()
            .map(|thread| thread.task_budget.clone())
            .unwrap_or_default();
        let mut usage_total =
            am_db::repos::usage_ledger::total_for_session(&self.db.pool, &thread_id)
                .await
                .unwrap_or(0);
        let (mut enforcement_state, budget_state_invalid) =
            match am_db::repos::task_budget_state::get(&self.db.pool, &thread_id).await {
                Ok(value) => match crate::budget::EnforcementState::from_json(value) {
                    Ok(state) => (state, false),
                    Err(_) => (crate::budget::EnforcementState::default(), true),
                },
                Err(_) => (crate::budget::EnforcementState::default(), true),
            };
        let usage_model = usage_turn.as_ref().and_then(|turn| turn.model.clone());
        let usage_policy_envelope_id = usage_turn
            .as_ref()
            .and_then(|turn| turn.policy_envelope_id.clone());

        while let Some(event) = events.recv().await {
            if let NormalizedEvent::AssistantTextDelta { delta } = &event {
                if delta.is_empty() {
                    continue;
                }
                let mut streamed = if let Some(mut streamed) = streaming_assistant.take() {
                    streamed
                        .text
                        .get_or_insert_with(String::new)
                        .push_str(delta);
                    streamed
                } else {
                    map_thread_event(&thread_id, &turn_id, &event)
                };
                streamed.data = json!({ "streaming": true });
                let _ = am_db::repos::agent_thread_message::upsert(&self.db.pool, &streamed).await;
                self.events
                    .publish(AppEvent::AgentThreadEvent(streamed.clone()));
                streaming_assistant = Some(streamed);
                continue;
            }

            if let NormalizedEvent::AssistantText { text } = &event {
                if let Some(mut streamed) = streaming_assistant.take() {
                    streamed.text = Some(text.clone());
                    streamed.data = json!({ "streaming": false });
                    let _ =
                        am_db::repos::agent_thread_message::upsert(&self.db.pool, &streamed).await;
                    self.events.publish(AppEvent::AgentThreadEvent(streamed));
                    continue;
                }
            }

            // Some provider failures can end a stream without a completed text
            // item. Preserve the accumulated text and remove its live caret.
            if matches!(event, NormalizedEvent::SessionEnded { .. }) {
                if let Some(mut streamed) = streaming_assistant.take() {
                    streamed.data = json!({ "streaming": false });
                    let _ =
                        am_db::repos::agent_thread_message::upsert(&self.db.pool, &streamed).await;
                    self.events.publish(AppEvent::AgentThreadEvent(streamed));
                }
            }

            let ended_status = match &event {
                NormalizedEvent::SessionEnded { status } => Some(*status),
                _ => None,
            };

            match &event {
                NormalizedEvent::AwaitingApproval { .. } => saw_approval_needed = true,
                NormalizedEvent::TokenUsage { input, output } => {
                    saw_token_telemetry = true;
                    let (input_delta, output_delta) = usage_reconciler.delta(*input, *output);
                    if input_delta == 0 && output_delta == 0 {
                        continue;
                    }
                    if let Ok(recorded) = self
                        .record_token_usage(
                            usage_project_id.clone(),
                            Some(thread_id.clone()),
                            Some(turn_id.clone()),
                            agent,
                            usage_model.clone(),
                            usage_policy_envelope_id.clone(),
                            input_delta,
                            output_delta,
                        )
                        .await
                    {
                        usage_total = usage_total.saturating_add(recorded);
                    }
                    if let TaskBudget::Tokens { limit_tokens } = &usage_budget {
                        let remaining = limit_tokens.saturating_sub(usage_total);
                        if usage_total >= *limit_tokens {
                            budget_exhausted = true;
                            let _ = self.sessions.cancel(&thread_id).await;
                        } else if remaining <= crate::budget::token_reserve(*limit_tokens)
                            && !enforcement_state.closeout_sent
                        {
                            enforcement_state.closeout_sent = true;
                            let _ = am_db::repos::task_budget_state::save(
                                &self.db.pool,
                                &thread_id,
                                &enforcement_state.to_json(),
                            )
                            .await;
                            let _ = self
                                .sessions
                                .steer(&thread_id, crate::budget::closeout_instruction())
                                .await;
                        }
                        if usage_total.saturating_mul(2) >= *limit_tokens
                            && !enforcement_state.reminder_sent
                        {
                            enforcement_state.reminder_sent = true;
                            let _ = am_db::repos::task_budget_state::save(
                                &self.db.pool,
                                &thread_id,
                                &enforcement_state.to_json(),
                            )
                            .await;
                            let _ = self
                                .sessions
                                .steer(&thread_id, crate::budget::progress_instruction())
                                .await;
                        }
                    }
                }
                NormalizedEvent::QuotaWindow {
                    window,
                    used_percent,
                    reset_at,
                } => {
                    match window {
                        QuotaWindowKind::FiveHour => saw_five_hour_telemetry = true,
                        QuotaWindowKind::Weekly => saw_weekly_telemetry = true,
                    }
                    let provider_usage =
                        self.update_provider_usage(agent, *window, *used_percent, *reset_at);
                    self.events.publish(AppEvent::ProviderUsageUpdated {
                        agent,
                        usage: provider_usage,
                    });
                    let consumed =
                        enforcement_state.observe(*window, *used_percent, agent.as_str());
                    let _ = am_db::repos::task_budget_state::save(
                        &self.db.pool,
                        &thread_id,
                        &enforcement_state.to_json(),
                    )
                    .await;
                    if let Some(limit) = crate::budget::quota_limit(&usage_budget, *window) {
                        let remaining = (limit - consumed).max(0.0);
                        if consumed >= limit {
                            budget_exhausted = true;
                            let _ = self.sessions.cancel(&thread_id).await;
                        } else if remaining <= limit * 0.15 && !enforcement_state.closeout_sent {
                            enforcement_state.closeout_sent = true;
                            let _ = am_db::repos::task_budget_state::save(
                                &self.db.pool,
                                &thread_id,
                                &enforcement_state.to_json(),
                            )
                            .await;
                            let _ = self
                                .sessions
                                .steer(&thread_id, crate::budget::closeout_instruction())
                                .await;
                        }
                        if consumed >= limit * 0.5 && !enforcement_state.reminder_sent {
                            enforcement_state.reminder_sent = true;
                            let _ = am_db::repos::task_budget_state::save(
                                &self.db.pool,
                                &thread_id,
                                &enforcement_state.to_json(),
                            )
                            .await;
                            let _ = self
                                .sessions
                                .steer(&thread_id, crate::budget::progress_instruction())
                                .await;
                        }
                    }
                }
                NormalizedEvent::SessionStarted {
                    session_id: provider,
                } => {
                    let _ = am_db::repos::agent_turn::set_agent_session_id(
                        &self.db.pool,
                        &turn_id,
                        provider,
                    )
                    .await;
                }
                NormalizedEvent::UsageLimitReached { reset_at } => {
                    saw_usage_limit = true;
                    limit_reset_at = *reset_at;
                    let _ = self.mark_agent_limited(agent, *reset_at).await;
                    if let Ok(Some(mut thread)) =
                        am_db::repos::agent_thread::get(&self.db.pool, &thread_id).await
                    {
                        let percentage_budget = crate::budget::is_percentage_budget(&usage_budget);
                        thread.status = if percentage_budget {
                            TaskStatus::Paused
                        } else {
                            TaskStatus::WaitingForLimit
                        };
                        thread.limit_reset_at = *reset_at;
                        thread.handoff_state = if percentage_budget {
                            "budget_paused_provider_limit".to_string()
                        } else {
                            "waiting_for_fallback".to_string()
                        };
                        if let Ok(saved) =
                            am_db::repos::agent_thread::save(&self.db.pool, &thread).await
                        {
                            self.events.publish(AppEvent::AgentThreadUpdated(saved));
                        }
                    }
                    let _ = self
                        .activity(
                            None,
                            None,
                            "thread.agent_limited",
                            json!({
                                "thread_id": thread_id,
                                "agent": agent.as_str(),
                                "reset_at": reset_at,
                            }),
                        )
                        .await;
                }
                NormalizedEvent::NetworkUnavailable { message } => {
                    saw_network_loss = true;
                    if let Ok(Some(mut thread)) =
                        am_db::repos::agent_thread::get(&self.db.pool, &thread_id).await
                    {
                        thread.status = TaskStatus::WaitingForNetwork;
                        thread.handoff_state = "waiting_for_network".to_string();
                        if let Ok(saved) =
                            am_db::repos::agent_thread::save(&self.db.pool, &thread).await
                        {
                            self.events.publish(AppEvent::AgentThreadUpdated(saved));
                        }
                    }
                    let _ = self
                        .activity(
                            None,
                            None,
                            "thread.network_unavailable",
                            json!({
                                "thread_id": thread_id,
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
                    let _ = am_db::repos::agent_turn::finish(&self.db.pool, &turn_id, state).await;
                    let _ = am_db::repos::work_graph::finish_runs_for_ref(
                        &self.db.pool,
                        &turn_id,
                        state,
                    )
                    .await;
                    if effective_status == SessionStatus::Completed && !saw_usage_limit {
                        let _ = self.mark_agent_available(agent).await;
                    }

                    if let Ok(Some(mut thread)) =
                        am_db::repos::agent_thread::get(&self.db.pool, &thread_id).await
                    {
                        let budget_telemetry_complete = match &usage_budget {
                            TaskBudget::Unlimited => true,
                            TaskBudget::Tokens { .. } => saw_token_telemetry,
                            TaskBudget::WeeklyPercent { .. } => saw_weekly_telemetry,
                            TaskBudget::ClaudePercent {
                                five_hour_percent,
                                weekly_percent,
                            } => {
                                five_hour_percent.is_none_or(|_| saw_five_hour_telemetry)
                                    && weekly_percent.is_none_or(|_| saw_weekly_telemetry)
                            }
                        };
                        thread.status = if budget_exhausted
                            || (!usage_budget.is_unlimited()
                                && (!budget_telemetry_complete || budget_state_invalid))
                        {
                            TaskStatus::Paused
                        } else if saw_network_loss {
                            TaskStatus::WaitingForNetwork
                        } else if saw_usage_limit {
                            TaskStatus::WaitingForLimit
                        } else {
                            match effective_status {
                                SessionStatus::Completed if saw_approval_needed => {
                                    TaskStatus::AwaitingApproval
                                }
                                SessionStatus::Completed => TaskStatus::Review,
                                SessionStatus::Interrupted => TaskStatus::Paused,
                                SessionStatus::Failed => TaskStatus::Failed,
                            }
                        };
                        if saw_usage_limit {
                            thread.limit_reset_at = limit_reset_at;
                        }
                        if let Ok(saved) =
                            am_db::repos::agent_thread::save(&self.db.pool, &thread).await
                        {
                            self.events.publish(AppEvent::AgentThreadUpdated(saved));
                        }
                    }
                }
                _ => {}
            }

            if !matches!(event, NormalizedEvent::QuotaWindow { .. })
                && (usage_budget.is_unlimited()
                    || !matches!(event, NormalizedEvent::TokenUsage { .. }))
            {
                let ev = map_thread_event(&thread_id, &turn_id, &event);
                let _ = am_db::repos::agent_thread_message::insert(&self.db.pool, &ev).await;
                self.events.publish(AppEvent::AgentThreadEvent(ev));
            }

            if let Some(status) = ended_status {
                let handoff_status = if saw_network_loss {
                    SessionStatus::Interrupted
                } else {
                    status
                };
                let _ = self
                    .apply_thread_handoff(&thread_id, &turn_id, agent, handoff_status)
                    .await;
            }
        }

        self.cancel_session_approvals(&turn_id).await;
        self.sessions.remove(&thread_id).await;
        drop(permit);
        self.sandboxes.release(sandbox_lease).await;
        let _ = am_db::repos::work_graph::release_locks_for_thread(&self.db.pool, &thread_id).await;
        // Capacity and repo locks just freed: queued work can start now.
        self.wake_scheduler();
        if let Some(project_id) = usage_project_id.as_deref() {
            self.notify_plan_watchers(project_id);
        }

        // The turn was interrupted (limit/network) before it could act on the
        // user's input. Put that input back on the queue so the fallback agent or
        // the scheduler's resume delivers it, instead of dropping it and resuming
        // with a generic "continue" prompt.
        if !budget_exhausted && (saw_usage_limit || saw_network_loss) && pending_message.is_some() {
            if let Some(msg) = pending_message.as_ref() {
                let permission = am_db::repos::agent_thread::get(&self.db.pool, &thread_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|t| t.permission)
                    .unwrap_or_else(|| permission_to_string(PermissionPolicy::default()));
                // The interrupted turn is older than follow-ups that arrived
                // while it was running. Put it back at the head so a limit or
                // network interruption cannot make later messages overtake
                // the user's original request.
                let existing_ids =
                    am_db::repos::queued_turn::list_for_thread(&self.db.pool, &thread_id)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|queued| queued.id)
                        .collect::<Vec<_>>();
                let queued = am_db::repos::queued_turn::enqueue_with_echo(
                    &self.db.pool,
                    &thread_id,
                    agent,
                    &permission,
                    &msg.text,
                    None,
                    false,
                    msg.client_message_id.as_deref(),
                )
                .await;
                if let Ok(queued) = queued {
                    let mut ordered_ids = vec![queued.id];
                    ordered_ids.extend(existing_ids);
                    let _ =
                        am_db::repos::queued_turn::reorder(&self.db.pool, &thread_id, &ordered_ids)
                            .await;
                }
            }
        }

        let budget_telemetry_complete = match &usage_budget {
            TaskBudget::Unlimited => true,
            TaskBudget::Tokens { .. } => saw_token_telemetry,
            TaskBudget::WeeklyPercent { .. } => saw_weekly_telemetry,
            TaskBudget::ClaudePercent {
                five_hour_percent,
                weekly_percent,
            } => {
                five_hour_percent.is_none_or(|_| saw_five_hour_telemetry)
                    && weekly_percent.is_none_or(|_| saw_weekly_telemetry)
            }
        };
        if budget_exhausted
            || (!usage_budget.is_unlimited()
                && (!budget_telemetry_complete || budget_state_invalid))
        {
            return;
        }

        if saw_network_loss && !usage_budget.is_unlimited() {
            return;
        }

        if saw_network_loss {
            self.handle_thread_network_loss(&thread_id, agent).await;
            return;
        }

        if saw_usage_limit {
            if crate::budget::is_percentage_budget(&usage_budget) {
                return;
            }
            self.start_thread_fallback(&thread_id, agent, limit_reset_at)
                .await;
            return;
        }

        if completed_ok {
            self.maybe_mark_thread_switchback_ready(&thread_id).await;
        }

        if let Ok(Some(next)) = am_db::repos::queued_turn::pop_next(&self.db.pool, &thread_id).await
        {
            // A queued message records the agent selected when it was sent,
            // but an automatic switchback may have completed while the
            // previous turn was draining. Re-read the thread so the queued
            // continuation follows the resolved active agent instead of
            // immediately re-entering the fallback provider.
            let next_agent = am_db::repos::agent_thread::get(&self.db.pool, &thread_id)
                .await
                .ok()
                .flatten()
                .and_then(|thread| thread.active_agent)
                .unwrap_or(next.agent_kind);
            let core = self.clone();
            tokio::spawn(async move {
                let permission = parse_permission(&next.permission);
                let _ = core
                    .run_agent_thread_boxed(
                        &thread_id,
                        next_agent,
                        permission,
                        Some(PendingThreadMessage::from_queued(next)),
                    )
                    .await;
            });
        }
    }

    fn run_agent_thread_boxed<'a>(
        &'a self,
        thread_id: &'a str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<PendingThreadMessage>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, CoreError>> + Send + 'a>>
    {
        Box::pin(self.run_agent_thread_inner(thread_id, agent, permission, message, None))
    }

    async fn known_limited_agent_reset(
        &self,
        agent: AgentKind,
    ) -> Result<Option<Option<chrono::DateTime<chrono::Utc>>>, CoreError> {
        let Some(record) = am_db::repos::agent::get(&self.db.pool, agent).await? else {
            return Ok(None);
        };
        if record.availability != AvailabilityState::Limited {
            return Ok(None);
        }
        if record
            .reset_at
            .is_some_and(|reset_at| reset_at <= chrono::Utc::now())
        {
            return Ok(None);
        }
        Ok(Some(record.reset_at))
    }

    async fn start_known_limited_thread_fallback(
        &self,
        mut thread: AgentThread,
        current: AgentKind,
        permission: PermissionPolicy,
        message: Option<PendingThreadMessage>,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
        policy_envelope_id: Option<String>,
    ) -> Result<String, CoreError> {
        let thread_id = thread.id.clone();
        let permission_string = permission_to_string(permission);
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.agent_limited",
            json!({
                "thread_id": thread_id,
                "agent": current.as_str(),
                "reset_at": reset_at,
                "reason": "preflight",
            }),
        )
        .await?;

        match self.fallback_decision(current, reset_at).await? {
            crate::fallback::FallbackDecision::Switch { agent, switch_back } => {
                if thread.original_agent.is_none() {
                    thread.original_agent = Some(current);
                    thread.original_model = thread.model.clone();
                    thread.original_local_provider = thread.local_provider;
                    thread.original_local_base_url = thread.local_base_url.clone();
                }
                thread.fallback_agent = Some(agent);
                thread.active_agent = Some(agent);
                let next_model = thread
                    .model
                    .clone()
                    .filter(|m| model_compatible_with_agent(agent, m));
                thread.model = next_model.clone();
                thread.fallback_model = next_model;
                thread.local_provider = None;
                thread.local_base_url = None;
                thread.execution_backend =
                    fallback_backend_for_agent(agent, thread.execution_backend);
                thread.status = TaskStatus::Queued;
                thread.switch_back = switch_back;
                thread.limit_reset_at = reset_at;
                thread.handoff_state = "fallback_active".to_string();
                let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events
                    .publish(AppEvent::AgentThreadUpdated(saved.clone()));
                self.activity(
                    saved.project_id.clone(),
                    None,
                    "thread.fallback_started",
                    json!({
                        "thread_id": thread_id,
                        "from": current.as_str(),
                        "to": agent.as_str(),
                        "reset_at": reset_at,
                        "reason": "known_limited",
                    }),
                )
                .await?;
                self.run_agent_thread_boxed(&thread_id, agent, permission, message)
                    .await
            }
            crate::fallback::FallbackDecision::Wait { reset_at } => {
                let local_policy = self.get_local_model_policy().await.unwrap_or_default();
                if thread.task_budget.is_unlimited() {
                    if let Ok(Some(target)) = self.best_ready_local_target(&local_policy).await {
                        let queued_id = self
                            .queue_known_limited_message(
                                &thread_id,
                                current,
                                &permission_string,
                                message,
                                policy_envelope_id.as_deref(),
                            )
                            .await?;
                        self.start_thread_local_fallback(
                            &thread_id,
                            current,
                            target,
                            &local_policy,
                        )
                        .await;
                        return Ok(queued_id.unwrap_or(thread_id));
                    }
                }
                let queued_id = self
                    .queue_known_limited_message(
                        &thread_id,
                        current,
                        &permission_string,
                        message,
                        policy_envelope_id.as_deref(),
                    )
                    .await?;
                thread.status = TaskStatus::WaitingForLimit;
                thread.limit_reset_at = reset_at;
                thread.handoff_state = "waiting_for_reset".to_string();
                let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events
                    .publish(AppEvent::AgentThreadUpdated(saved.clone()));
                self.activity(
                    saved.project_id,
                    None,
                    "thread.fallback_waiting",
                    json!({
                        "thread_id": thread_id,
                        "agent": current.as_str(),
                        "reset_at": reset_at,
                        "queue_id": queued_id,
                        "reason": "no_ready_fallback",
                    }),
                )
                .await?;
                Ok(queued_id.unwrap_or(thread_id))
            }
            crate::fallback::FallbackDecision::Disabled => {
                let queued_id = self
                    .queue_known_limited_message(
                        &thread_id,
                        current,
                        &permission_string,
                        message,
                        policy_envelope_id.as_deref(),
                    )
                    .await?;
                thread.status = TaskStatus::WaitingForLimit;
                thread.limit_reset_at = reset_at;
                thread.handoff_state = "waiting_for_reset".to_string();
                let saved = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events
                    .publish(AppEvent::AgentThreadUpdated(saved.clone()));
                self.activity(
                    saved.project_id,
                    None,
                    "thread.fallback_disabled",
                    json!({
                        "thread_id": thread_id,
                        "agent": current.as_str(),
                        "reset_at": reset_at,
                        "queue_id": queued_id,
                        "reason": "auto_switch_disabled",
                    }),
                )
                .await?;
                Ok(queued_id.unwrap_or(thread_id))
            }
        }
    }

    async fn queue_known_limited_message(
        &self,
        thread_id: &str,
        agent: AgentKind,
        permission: &str,
        message: Option<PendingThreadMessage>,
        policy_envelope_id: Option<&str>,
    ) -> Result<Option<String>, CoreError> {
        let Some(message) = message else {
            return Ok(None);
        };
        let text = message.text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let queued = am_db::repos::queued_turn::enqueue_with_echo(
            &self.db.pool,
            thread_id,
            agent,
            permission,
            text,
            policy_envelope_id,
            message.echo_user_message,
            message.client_message_id.as_deref(),
        )
        .await?;
        Ok(Some(queued.id))
    }

    async fn start_thread_fallback(
        &self,
        thread_id: &str,
        current: AgentKind,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        let Ok(decision) = self.fallback_decision(current, reset_at).await else {
            return;
        };
        match decision {
            crate::fallback::FallbackDecision::Switch { agent, switch_back } => {
                let Ok(Some(mut thread)) =
                    am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
                else {
                    return;
                };
                if thread.original_agent.is_none() {
                    thread.original_agent = Some(current);
                    thread.original_model = thread.model.clone();
                    thread.original_local_provider = thread.local_provider;
                    thread.original_local_base_url = thread.local_base_url.clone();
                }
                thread.fallback_agent = Some(agent);
                thread.active_agent = Some(agent);
                // Drop a model that belongs to the previous agent so the fallback
                // agent runs (and is shown) with a compatible model instead of an
                // incompatible id like `gpt-5.5` on Claude. None lets the CLI pick
                // its own default; switch-back restores the original model.
                let next_model = thread
                    .model
                    .clone()
                    .filter(|m| model_compatible_with_agent(agent, m));
                thread.model = next_model.clone();
                thread.fallback_model = next_model;
                thread.local_provider = None;
                thread.local_base_url = None;
                thread.execution_backend =
                    fallback_backend_for_agent(agent, thread.execution_backend);
                thread.status = TaskStatus::Queued;
                thread.switch_back = switch_back;
                thread.limit_reset_at = reset_at;
                thread.handoff_state = "fallback_active".to_string();
                let permission = parse_permission(&thread.permission);
                if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &thread).await {
                    self.events
                        .publish(AppEvent::AgentThreadUpdated(saved.clone()));
                    let _ = self
                        .activity(
                            saved.project_id.clone(),
                            None,
                            "thread.fallback_started",
                            json!({
                                "thread_id": thread_id,
                                "from": current.as_str(),
                                "to": agent.as_str(),
                            }),
                        )
                        .await;
                }
                let core = self.clone();
                let tid = thread_id.to_string();
                tokio::spawn(async move {
                    let _ = core
                        .run_agent_thread_boxed(&tid, agent, permission, None)
                        .await;
                });
            }
            crate::fallback::FallbackDecision::Wait { reset_at } => {
                let local_policy = self.get_local_model_policy().await.unwrap_or_default();
                if let Ok(Some(target)) = self.best_ready_local_target(&local_policy).await {
                    self.start_thread_local_fallback(thread_id, current, target, &local_policy)
                        .await;
                    return;
                }
                if let Ok(Some(mut thread)) =
                    am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
                {
                    thread.status = TaskStatus::WaitingForLimit;
                    thread.limit_reset_at = reset_at;
                    thread.handoff_state = "waiting_for_reset".to_string();
                    if let Ok(saved) =
                        am_db::repos::agent_thread::save(&self.db.pool, &thread).await
                    {
                        self.events
                            .publish(AppEvent::AgentThreadUpdated(saved.clone()));
                        let _ = self
                            .activity(
                                saved.project_id,
                                None,
                                "thread.fallback_waiting",
                                json!({
                                    "thread_id": thread_id,
                                    "agent": current.as_str(),
                                    "reset_at": reset_at,
                                    "reason": "no_ready_fallback",
                                }),
                            )
                            .await;
                    }
                }
            }
            crate::fallback::FallbackDecision::Disabled => {
                if let Ok(Some(thread)) =
                    am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
                {
                    let _ = self
                        .activity(
                            thread.project_id,
                            None,
                            "thread.fallback_disabled",
                            json!({
                                "thread_id": thread_id,
                                "agent": current.as_str(),
                                "reset_at": reset_at,
                                "reason": "auto_switch_disabled",
                            }),
                        )
                        .await;
                }
            }
        }
    }

    async fn handle_thread_network_loss(&self, thread_id: &str, current: AgentKind) {
        let policy = self.get_local_model_policy().await.unwrap_or_default();
        if policy.offline_grace_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(policy.offline_grace_secs)).await;
        }

        if policy.auto_resume_cloud {
            match self.cloud_connectivity_stable(&policy).await {
                Ok(true) => {
                    self.resume_thread_cloud_after_network(thread_id, current, "network.restored")
                        .await;
                    return;
                }
                Ok(false) => {}
                Err(err) => {
                    let _ = self
                        .activity(
                            None,
                            None,
                            "thread.network_probe_failed",
                            json!({ "thread_id": thread_id, "error": err.to_string() }),
                        )
                        .await;
                }
            }
        }

        if policy.use_local_fallback {
            match self.best_ready_local_target(&policy).await {
                Ok(Some(target)) => {
                    self.start_thread_local_fallback(thread_id, current, target, &policy)
                        .await;
                }
                Ok(None) => {
                    let _ = self
                        .activity(
                            None,
                            None,
                            "thread.network_waiting",
                            json!({ "thread_id": thread_id, "agent": current.as_str() }),
                        )
                        .await;
                }
                Err(err) => {
                    let _ = self
                        .activity(
                            None,
                            None,
                            "thread.local_fallback_failed",
                            json!({ "thread_id": thread_id, "error": err.to_string() }),
                        )
                        .await;
                }
            }
        }
    }

    pub(crate) async fn start_thread_local_fallback(
        &self,
        thread_id: &str,
        current: AgentKind,
        target: am_proto::LocalModelTarget,
        policy: &am_proto::LocalModelPolicy,
    ) {
        let Ok(Some(mut thread)) = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
        else {
            return;
        };
        if self.sessions.is_active(thread_id).await {
            thread.switch_back_pending = true;
            let _ = am_db::repos::agent_thread::save(&self.db.pool, &thread).await;
            return;
        }

        if thread.original_agent.is_none() {
            thread.original_agent = Some(current);
            thread.original_model = thread.model.clone();
            thread.original_local_provider = thread.local_provider;
            thread.original_local_base_url = thread.local_base_url.clone();
        }
        let target_provider = target.provider;
        let target_model = target.model.clone();
        let target_base_url = target.base_url.clone();
        let runtime_probe = am_agents::LocalModelRuntime {
            provider: target_provider,
            model: target_model.clone(),
            base_url: target_base_url.clone(),
            api_token: None,
        };
        let backend_adjusted = thread.execution_backend == ExecutionBackend::DockerSandbox
            && local_model_uses_container_localhost(&runtime_probe);
        if backend_adjusted {
            thread.execution_backend = ExecutionBackend::Host;
        }
        thread.fallback_agent = Some(AgentKind::Codex);
        thread.fallback_model = Some(target_model.clone());
        thread.fallback_local_provider = Some(target_provider);
        thread.fallback_local_base_url = target_base_url.clone();
        thread.active_agent = Some(AgentKind::Codex);
        thread.model = Some(target_model);
        thread.local_provider = Some(target_provider);
        thread.local_base_url = target_base_url;
        thread.status = TaskStatus::Queued;
        thread.switch_back = policy.switch_back_to_cloud;
        thread.switch_back_pending = false;
        thread.handoff_state = "local_fallback_active".to_string();
        let permission = parse_permission(&thread.permission);

        if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &thread).await {
            self.events
                .publish(AppEvent::AgentThreadUpdated(saved.clone()));
            let _ = self
                .activity(
                    saved.project_id.clone(),
                    None,
                    "thread.local_fallback_started",
                    json!({
                        "thread_id": thread_id,
                        "from": current.as_str(),
                        "to": AgentKind::Codex.as_str(),
                        "provider": saved.local_provider.map(|provider| provider.as_str()),
                        "model": saved.model,
                        "backend_adjusted": backend_adjusted,
                    }),
                )
                .await;
        }

        let core = self.clone();
        let tid = thread_id.to_string();
        tokio::spawn(async move {
            let _ = core
                .run_agent_thread_boxed(&tid, AgentKind::Codex, permission, None)
                .await;
        });
    }

    pub(crate) async fn resume_thread_cloud_after_network(
        &self,
        thread_id: &str,
        current: AgentKind,
        activity_kind: &str,
    ) {
        let Ok(Some(mut thread)) = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
        else {
            return;
        };
        if self.sessions.is_active(thread_id).await {
            thread.switch_back_pending = true;
            if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &thread).await {
                self.events.publish(AppEvent::AgentThreadUpdated(saved));
            }
            return;
        }

        let had_original = thread.original_agent.is_some();
        let agent = thread.original_agent.unwrap_or(current);
        thread.active_agent = Some(agent);
        if had_original {
            thread.model = thread.original_model.clone();
            thread.local_provider = thread.original_local_provider;
            thread.local_base_url = thread.original_local_base_url.clone();
        }
        thread.fallback_agent = None;
        thread.fallback_model = None;
        thread.fallback_local_provider = None;
        thread.fallback_local_base_url = None;
        thread.original_agent = None;
        thread.original_model = None;
        thread.original_local_provider = None;
        thread.original_local_base_url = None;
        thread.switch_back_pending = false;
        thread.status = TaskStatus::Queued;
        thread.handoff_state = "network_restored".to_string();
        let permission = parse_permission(&thread.permission);

        if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &thread).await {
            self.events
                .publish(AppEvent::AgentThreadUpdated(saved.clone()));
            let _ = self
                .activity(
                    saved.project_id.clone(),
                    None,
                    activity_kind,
                    json!({ "thread_id": thread_id, "agent": agent.as_str() }),
                )
                .await;
        }

        let core = self.clone();
        let tid = thread_id.to_string();
        tokio::spawn(async move {
            let _ = core
                .run_agent_thread_boxed(&tid, agent, permission, None)
                .await;
        });
    }

    async fn maybe_mark_thread_switchback_ready(&self, thread_id: &str) {
        let Ok(Some(mut thread)) = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await
        else {
            return;
        };
        let Some(original) = thread.original_agent else {
            return;
        };
        if !thread.switch_back {
            return;
        }
        if thread.fallback_local_provider.is_some() {
            let policy = self.get_local_model_policy().await.unwrap_or_default();
            if !policy.switch_back_to_cloud {
                return;
            }
            match self.cloud_connectivity_stable(&policy).await {
                Ok(true) => {
                    self.resume_thread_cloud_after_network(
                        thread_id,
                        original,
                        "thread.local_switchback_started",
                    )
                    .await;
                }
                Ok(false) => {
                    thread.switch_back_pending = true;
                    thread.status = TaskStatus::WaitingForNetwork;
                    thread.handoff_state = "local_fallback_waiting_for_cloud".to_string();
                    if let Ok(saved) =
                        am_db::repos::agent_thread::save(&self.db.pool, &thread).await
                    {
                        self.events.publish(AppEvent::AgentThreadUpdated(saved));
                    }
                }
                Err(_) => {}
            }
            return;
        }
        if thread
            .limit_reset_at
            .is_some_and(|reset_at| reset_at > chrono::Utc::now())
        {
            return;
        }
        let project_id = thread.project_id.clone();
        let from = thread.fallback_agent.or(thread.active_agent);
        let _ = self
            .activity(
                project_id.clone(),
                None,
                "thread.switchback_started",
                json!({
                    "thread_id": thread_id,
                    "from": from.map(|agent| agent.as_str()),
                    "to": original.as_str(),
                }),
            )
            .await;
        thread.active_agent = Some(original);
        thread.fallback_agent = None;
        thread.model = thread.original_model.clone();
        thread.local_provider = thread.original_local_provider;
        thread.local_base_url = thread.original_local_base_url.clone();
        thread.original_agent = None;
        thread.original_model = None;
        thread.original_local_provider = None;
        thread.original_local_base_url = None;
        thread.fallback_model = None;
        thread.fallback_local_provider = None;
        thread.fallback_local_base_url = None;
        thread.switch_back_pending = false;
        thread.limit_reset_at = None;
        thread.handoff_state = "resolved".to_string();
        if let Ok(saved) = am_db::repos::agent_thread::save(&self.db.pool, &thread).await {
            self.events.publish(AppEvent::AgentThreadUpdated(saved));
            let _ = self
                .activity(
                    project_id,
                    None,
                    "thread.switchback_completed",
                    json!({
                        "thread_id": thread_id,
                        "agent": original.as_str(),
                    }),
                )
                .await;
        }
    }

    async fn latest_thread_session_ref(
        &self,
        thread_id: &str,
        agent: AgentKind,
        target_hash: &str,
        legacy_target_hash: &str,
    ) -> Result<Option<SessionRef>, CoreError> {
        let turns = am_db::repos::agent_turn::list_for_thread(&self.db.pool, thread_id).await?;
        Ok(turns.into_iter().rev().find_map(|turn| {
            let target_matches =
                target_hash_matches(turn.target_hash.as_deref(), target_hash, legacy_target_hash);
            if turn.agent_kind == agent && target_matches {
                turn.agent_session_id
                    .map(|agent_session_id| SessionRef { agent_session_id })
            } else {
                None
            }
        }))
    }

    async fn thread_execution_backend(
        &self,
        thread: &AgentThread,
        requested: Option<ExecutionBackend>,
    ) -> Result<ExecutionBackend, CoreError> {
        if let Some(requested) = requested {
            return Ok(requested);
        }
        let links =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &thread.id).await?;
        if links.iter().any(|link| link.worktree_path.is_some()) {
            return Ok(thread.execution_backend);
        }
        Ok(thread.execution_backend)
    }

    async fn ensure_thread_workspace(
        &self,
        thread: &AgentThread,
        backend: ExecutionBackend,
        permission: PermissionPolicy,
    ) -> Result<ThreadWorkspace, CoreError> {
        let links =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &thread.id).await?;
        let visible_repo_workspace = self
            .visible_repo_workspace(&links, backend, permission)
            .await?;
        let workspace = visible_repo_workspace
            .clone()
            .unwrap_or_else(|| self.thread_workspace_path(&thread.id, backend));
        if visible_repo_workspace.is_none() {
            tokio::fs::create_dir_all(&workspace)
                .await
                .map_err(|e| CoreError::Other(e.to_string()))?;
        }
        for link in links {
            let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id)
                .await?
                .ok_or(CoreError::NotFound)?;
            let repo_path = repo
                .local_path
                .clone()
                .ok_or_else(|| CoreError::Other("repository has no local path".into()))?;
            let repo_dir_name = safe_repo_dir(&repo.name, &repo.id);
            let worktree = if visible_repo_workspace.is_some() {
                PathBuf::from(&repo_path)
            } else {
                workspace.join(repo_dir_name)
            };
            if link.worktree_path.as_ref().is_some_and(|path| {
                link.workspace_backend == backend
                    && Path::new(path).exists()
                    && same_path(Path::new(path), &worktree)
            }) {
                continue;
            }
            let short_thread = thread.id.split('-').next().unwrap_or("thread");
            let short_repo = repo.id.split('-').next().unwrap_or("repo");
            let branch = format!("am/thread-{short_thread}-{short_repo}");

            let repo_path_for_blocking = repo_path.clone();
            let worktree_for_blocking = worktree.clone();
            let branch_for_blocking = branch.clone();
            let use_visible_repo = visible_repo_workspace.is_some();
            let base_ref =
                tokio::task::spawn_blocking(move || -> Result<String, am_vcs::VcsError> {
                    let base = am_vcs::head_sha(Path::new(&repo_path_for_blocking))?;
                    if !use_visible_repo {
                        match backend {
                            // Cloud legs checkpoint from and reclaim into the
                            // host worktree, so they share its workspace shape.
                            ExecutionBackend::Host | ExecutionBackend::Cloud => {
                                am_vcs::create_worktree(
                                    Path::new(&repo_path_for_blocking),
                                    &worktree_for_blocking,
                                    &branch_for_blocking,
                                    &base,
                                )?;
                            }
                            ExecutionBackend::DockerSandbox => {
                                am_vcs::create_clone_workspace(
                                    Path::new(&repo_path_for_blocking),
                                    &worktree_for_blocking,
                                    &branch_for_blocking,
                                    &base,
                                )?;
                            }
                        }
                    }
                    Ok(base)
                })
                .await
                .map_err(|e| CoreError::Other(e.to_string()))?
                .map_err(|e| CoreError::Other(e.to_string()))?;

            let branch = if visible_repo_workspace.is_some() {
                am_vcs::validate_repo(&repo_path)
                    .map(|info| info.default_branch)
                    .unwrap_or_else(|_| "HEAD".to_string())
            } else {
                branch
            };
            am_db::repos::agent_thread_repo::upsert(
                &self.db.pool,
                &thread.id,
                &repo.id,
                Some(&worktree.to_string_lossy()),
                Some(&branch),
                Some(&base_ref),
                backend,
            )
            .await?;
        }

        Ok(ThreadWorkspace {
            path: workspace,
            uses_visible_repo: visible_repo_workspace.is_some(),
        })
    }

    async fn visible_repo_workspace(
        &self,
        links: &[am_proto::AgentThreadRepo],
        backend: ExecutionBackend,
        permission: PermissionPolicy,
    ) -> Result<Option<PathBuf>, CoreError> {
        if backend != ExecutionBackend::Host || links.len() != 1 {
            return Ok(None);
        }

        let link = &links[0];
        let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let Some(local_path) = repo.local_path else {
            return Err(CoreError::Other("repository has no local path".into()));
        };
        let repo_path = PathBuf::from(local_path);

        if link.workspace_backend == ExecutionBackend::Host
            && link
                .worktree_path
                .as_deref()
                .is_some_and(|path| same_path(Path::new(path), &repo_path))
        {
            return Ok(Some(repo_path));
        }

        if permission == PermissionPolicy::ReadOnly {
            return Ok(None);
        }

        Ok(Some(repo_path))
    }

    pub(crate) fn thread_workspace_path(
        &self,
        thread_id: &str,
        backend: ExecutionBackend,
    ) -> PathBuf {
        match backend {
            ExecutionBackend::Host | ExecutionBackend::Cloud => {
                self.data_dir.join("workbench").join(thread_id)
            }
            ExecutionBackend::DockerSandbox => self
                .data_dir
                .join("sandbox-workspaces")
                .join("workbench")
                .join(thread_id),
        }
    }

    pub(crate) async fn render_thread_context_files(
        &self,
        thread: &AgentThread,
        workspace: &Path,
    ) -> Result<(), CoreError> {
        let repos =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &thread.id).await?;
        let block = render_thread_context(thread, &repos);
        if !self.is_visible_repo_path(&repos, workspace).await? {
            for file in [THREAD_CONTEXT_FILE, CLAUDE_FILE, AGENTS_FILE] {
                write_text_if_changed(&workspace.join(file), &block).await?;
            }
        }
        for repo in repos {
            if let Some(path) = repo.worktree_path.as_deref() {
                let path = PathBuf::from(path);
                if path.exists()
                    && !self
                        .is_visible_repo_path(std::slice::from_ref(&repo), &path)
                        .await?
                {
                    for file in [CLAUDE_FILE, AGENTS_FILE] {
                        write_text_if_changed(&path.join(file), &block).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn is_visible_repo_path(
        &self,
        repos: &[am_proto::AgentThreadRepo],
        path: &Path,
    ) -> Result<bool, CoreError> {
        for link in repos {
            let Some(repo) = am_db::repos::repo::get(&self.db.pool, &link.repo_id).await? else {
                continue;
            };
            if let Some(local_path) = repo.local_path {
                if same_path(path, Path::new(&local_path)) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn apply_thread_handoff(
        &self,
        thread_id: &str,
        turn_id: &str,
        agent: AgentKind,
        status: SessionStatus,
    ) -> Result<(), CoreError> {
        let Some(mut thread) = am_db::repos::agent_thread::get(&self.db.pool, thread_id).await?
        else {
            return Ok(());
        };
        let events =
            am_db::repos::agent_thread_message::list_for_turn(&self.db.pool, turn_id).await?;
        let diff = self.thread_diff(thread_id).await.unwrap_or_default();
        let summary = build_thread_handoff_summary(agent, status, &events, &diff);
        thread.progress = append_thread_progress(&thread.progress, &summary);
        thread.next_actions = thread_next_actions(status, &events);
        let workspace = self.thread_workspace_path(thread_id, thread.execution_backend);
        let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
        let _ = self.render_thread_context_files(&thread, &workspace).await;
        self.activity(
            thread.project_id.clone(),
            None,
            "thread.context_handoff",
            json!({
                "thread_id": thread_id,
                "turn_id": turn_id,
                "agent": agent.as_str(),
                "status": status,
            }),
        )
        .await?;
        Ok(())
    }
}

pub(crate) fn permission_to_string(permission: PermissionPolicy) -> String {
    match permission {
        PermissionPolicy::ReadOnly => "read_only",
        PermissionPolicy::WorkspaceWrite => "workspace_write",
        PermissionPolicy::Ask => "ask",
        PermissionPolicy::Autonomous => "autonomous",
    }
    .to_string()
}

pub(crate) fn parse_permission(value: &str) -> PermissionPolicy {
    match value {
        "read_only" => PermissionPolicy::ReadOnly,
        "ask" => PermissionPolicy::Ask,
        "autonomous" => PermissionPolicy::Autonomous,
        _ => PermissionPolicy::WorkspaceWrite,
    }
}

/// Whether a stored model id can be used by `agent`. A blank model means "use
/// the CLI default" and is always compatible. Mirrors the frontend's
/// `modelCompatibleWithAgent` so an auto-switch doesn't carry over an
/// incompatible model id (e.g. a `gpt-*` model onto Claude).
fn model_compatible_with_agent(agent: AgentKind, model: &str) -> bool {
    let model = model.trim().to_lowercase();
    if model.is_empty() {
        return true;
    }
    match agent {
        AgentKind::Codex => !is_claude_model(&model),
        AgentKind::ClaudeCode => !is_codex_model(&model),
        _ => true,
    }
}

fn fallback_backend_for_agent(agent: AgentKind, backend: ExecutionBackend) -> ExecutionBackend {
    if backend == ExecutionBackend::DockerSandbox && agent != AgentKind::Codex {
        ExecutionBackend::Host
    } else {
        backend
    }
}

fn local_model_uses_container_localhost(local: &am_agents::LocalModelRuntime) -> bool {
    let base = local
        .base_url
        .as_deref()
        .unwrap_or_else(|| local.provider.default_base_url())
        .trim()
        .to_ascii_lowercase();
    base.contains("://127.")
        || base.contains("://localhost")
        || base.contains("://0.0.0.0")
        || base.starts_with("127.")
        || base.starts_with("localhost")
}

fn is_claude_model(model: &str) -> bool {
    matches!(model, "opus" | "sonnet" | "haiku" | "fable") || model.starts_with("claude-")
}

fn is_codex_model(model: &str) -> bool {
    model.contains("gpt-") || matches!(model.as_bytes(), [b'o', b'1'..=b'9', ..])
}

fn safe_repo_dir(name: &str, id: &str) -> String {
    let mut s = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    let prefix = if s.is_empty() { "repo" } else { s };
    format!("{}-{}", prefix, id.split('-').next().unwrap_or("repo"))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn direct_repo_diff_with_excludes(
    repo: &Path,
    base_sha: &str,
    max_bytes: usize,
    exclude_paths: &[&str],
) -> Result<TaskDiff, CoreError> {
    if !repo.exists() {
        return Ok(TaskDiff::default());
    }

    let name_status = git_read(
        repo,
        &git_diff_args(&["diff", "--name-status", base_sha], exclude_paths, false),
    )?;
    let numstat = git_read(
        repo,
        &git_diff_args(&["diff", "--numstat", base_sha], exclude_paths, false),
    )?;
    let mut patch = git_read(
        repo,
        &git_diff_args(
            &["diff", "--binary", "--full-index", base_sha],
            exclude_paths,
            false,
        ),
    )?;
    if patch.len() > max_bytes {
        patch.truncate(max_bytes);
        while !patch.is_char_boundary(patch.len()) {
            patch.pop();
        }
        patch.push_str("\n[diff truncated]\n");
    }

    let mut files = merge_file_changes(&name_status, &numstat);
    let untracked = git_read(
        repo,
        &git_diff_args(
            &["ls-files", "--others", "--exclude-standard"],
            exclude_paths,
            true,
        ),
    )
    .unwrap_or_default();
    for path in untracked
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        if files.iter().any(|file| file.path == path) {
            continue;
        }
        files.push(FileChange {
            path: path.to_string(),
            status: "untracked".to_string(),
            additions: 0,
            deletions: 0,
        });
    }

    Ok(TaskDiff {
        files,
        patch,
        repo_id: None,
        repo_name: None,
        remote_url: None,
        branch: git_read_static(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .filter(|value| !value.trim().is_empty()),
        base_ref: Some(base_sha.to_string()),
        head_ref: am_vcs::head_sha(repo).ok(),
        worktree_path: Some(repo.to_string_lossy().to_string()),
    })
}

fn git_diff_args(prefix: &[&str], exclude_paths: &[&str], include_pathspec: bool) -> Vec<String> {
    let mut args = prefix
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    if include_pathspec || !exclude_paths.is_empty() {
        args.push("--".to_string());
        args.push(".".to_string());
        args.extend(exclude_paths.iter().map(|path| format!(":(exclude){path}")));
    }
    args
}

fn git_read(repo: &Path, args: &[String]) -> Result<String, CoreError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| CoreError::Other(err.to_string()))?;
    if !output.status.success() {
        return Err(CoreError::Other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_read_static(repo: &Path, args: &[&str]) -> Result<String, CoreError> {
    let owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    git_read(repo, &owned)
}

fn merge_file_changes(name_status: &str, numstat: &str) -> Vec<FileChange> {
    let stats = parse_numstat(numstat);
    name_status
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            let status = parts.first().copied()?;
            let path = parts.last()?.to_string();
            let letter = status.chars().next().unwrap_or('M');
            let (additions, deletions) = stats
                .iter()
                .find(|(_, _, p)| *p == path)
                .map(|(a, d, _)| (*a, *d))
                .unwrap_or((0, 0));
            Some(FileChange {
                path,
                status: file_status_label(letter).to_string(),
                additions,
                deletions,
            })
        })
        .collect()
}

fn parse_numstat(output: &str) -> Vec<(u32, u32, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let additions = parts.next()?;
            let deletions = parts.next()?;
            let path = parts.next()?;
            let additions = additions.parse::<u32>().unwrap_or(0);
            let deletions = deletions.parse::<u32>().unwrap_or(0);
            Some((additions, deletions, path.to_string()))
        })
        .collect()
}

fn file_status_label(letter: char) -> &'static str {
    match letter {
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        _ => "modified",
    }
}

fn cleanup_managed_workspace(
    repo: &Path,
    worktree: &Path,
    branch: Option<&str>,
    backend: ExecutionBackend,
) -> Result<(), CoreError> {
    match backend {
        ExecutionBackend::Host | ExecutionBackend::Cloud => {
            am_vcs::remove_worktree(repo, worktree)
                .map_err(|err| CoreError::Other(err.to_string()))?;
            delete_managed_branch(repo, branch)?;
        }
        ExecutionBackend::DockerSandbox => {
            if worktree.exists() {
                std::fs::remove_dir_all(worktree).map_err(|err| {
                    CoreError::Other(format!(
                        "failed to remove managed workspace {}: {err}",
                        worktree.display()
                    ))
                })?;
            }
        }
    }
    Ok(())
}

fn delete_managed_branch(repo: &Path, branch: Option<&str>) -> Result<(), CoreError> {
    let Some(branch) = branch else {
        return Ok(());
    };
    if !branch.starts_with("am/thread-") {
        return Ok(());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "-D", branch])
        .output()
        .map_err(|err| CoreError::Other(err.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not found") || stderr.contains("branch not found") {
        return Ok(());
    }
    Err(CoreError::Other(stderr.trim().to_string()))
}

fn validate_runtime_budget(
    budget: &TaskBudget,
    agent: AgentKind,
    backend: ExecutionBackend,
    uses_local_model: bool,
) -> Result<(), CoreError> {
    if budget.is_unlimited() {
        return Ok(());
    }
    if backend != ExecutionBackend::Host || uses_local_model {
        return Err(CoreError::Other(
            "Task budgets require host execution with a hosted agent; local models and Docker Sandbox are not supported.".into(),
        ));
    }
    if matches!(budget, TaskBudget::WeeklyPercent { .. }) && agent != AgentKind::Codex {
        return Err(CoreError::Other(
            "Weekly % budgets currently require Codex with ChatGPT account usage telemetry.".into(),
        ));
    }
    if matches!(budget, TaskBudget::ClaudePercent { .. }) && agent != AgentKind::ClaudeCode {
        return Err(CoreError::Other(
            "Claude percentage budgets require Claude with rolling 5-hour or 7-day usage telemetry.".into(),
        ));
    }
    Ok(())
}

fn append_budget_instruction(prompt: &mut String, budget: &TaskBudget) {
    if let Some(instruction) = crate::budget::launch_instruction(budget) {
        prompt.push_str("\n\n[Internal session guidance]\n");
        prompt.push_str(&instruction);
    }
}

fn build_thread_initial_prompt(
    thread: &AgentThread,
    message: &str,
    context_files_available: bool,
) -> String {
    let mut prompt = if message.trim().is_empty() {
        thread.objective.clone()
    } else {
        message.to_string()
    };
    if prompt.trim().is_empty() {
        prompt = thread.title.clone();
    }
    if context_files_available {
        prompt.push_str(
            "\n\nBefore making changes, read TASK_CONTEXT.md and AGENTS.md in this workspace. Multiple repositories, if selected, are sibling directories under the current workspace root.",
        );
    } else {
        prompt.push_str(
            "\n\nUse the current repository working tree directly. Apply edits in place as you work.",
        );
    }
    append_budget_instruction(&mut prompt, &thread.task_budget);
    prompt
}

fn build_thread_resume_prompt(thread: &AgentThread, context_files_available: bool) -> String {
    let mut prompt = if context_files_available {
        format!(
            "Continue the Perpetual session \"{}\". Read TASK_CONTEXT.md and AGENTS.md first, then proceed from the recorded progress and next actions.",
            thread.title
        )
    } else {
        format!(
            "Continue the Perpetual session \"{}\" in the current repository working tree, applying edits in place as you work.",
            thread.title
        )
    };
    append_budget_instruction(&mut prompt, &thread.task_budget);
    prompt
}

fn build_thread_followup_prompt(thread: &AgentThread, message: &str) -> String {
    let mut prompt = message.to_string();
    append_budget_instruction(&mut prompt, &thread.task_budget);
    prompt
}

async fn write_text_if_changed(path: &Path, content: &str) -> Result<(), CoreError> {
    match tokio::fs::read_to_string(path).await {
        Ok(existing) if existing == content => return Ok(()),
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(CoreError::Other(format!(
                "failed to read {}: {err}",
                path.display()
            )));
        }
    }

    tokio::fs::write(path, content)
        .await
        .map_err(|err| CoreError::Other(format!("failed to write {}: {err}", path.display())))
}

fn render_thread_context(thread: &AgentThread, repos: &[am_proto::AgentThreadRepo]) -> String {
    let mut out = String::new();
    out.push_str("# Perpetual Session Context\n\n");
    out.push_str(&format!("Session: {}\n", thread.title));
    out.push_str(&format!("Updated: {}\n\n", now().to_rfc3339()));
    push_section(&mut out, "Objective", &thread.objective);
    out.push_str("## Repositories\n");
    if repos.is_empty() {
        out.push_str("None selected.\n\n");
    } else {
        for repo in repos {
            let dir = repo
                .worktree_path
                .as_deref()
                .and_then(|p| Path::new(p).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(&repo.repo_name);
            out.push_str(&format!("- {}: ./{}\n", repo.repo_name, dir));
        }
        out.push('\n');
    }
    push_section(&mut out, "Decisions", &thread.decisions);
    push_section(&mut out, "Progress", &thread.progress);
    push_section(&mut out, "Open Questions", &thread.open_questions);
    push_section(&mut out, "Next Actions", &thread.next_actions);
    out
}

fn push_section(out: &mut String, title: &str, value: &str) {
    out.push_str("## ");
    out.push_str(title);
    out.push('\n');
    if value.trim().is_empty() {
        out.push_str("None recorded.");
    } else {
        out.push_str(value.trim());
    }
    out.push_str("\n\n");
}

fn map_thread_event(thread_id: &str, turn_id: &str, ev: &NormalizedEvent) -> AgentThreadEvent {
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
            Some(compact_thread_event_detail(summary)),
            json!({ "ok": ok, "summary": capped_thread_event_detail(summary) }),
        ),
        NormalizedEvent::FileChanged { path, change } => (
            "app",
            "file_changed",
            Some(format!(
                "{} {}",
                thread_change_label(*change),
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
        NormalizedEvent::QuotaWindow { .. } => ("system", "quota_window", None, json!({})),
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

    AgentThreadEvent {
        id: new_id(),
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        role: role.to_string(),
        kind: kind.to_string(),
        text,
        client_message_id: None,
        data,
        ts: now(),
    }
}

fn compact_thread_event_detail(value: &str) -> String {
    let first_line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let compact = if first_line.trim().is_empty() {
        "Completed"
    } else {
        first_line.trim()
    };
    truncate_thread_chars(compact, 160)
}

fn capped_thread_event_detail(value: &str) -> String {
    truncate_thread_chars(value, MAX_EVENT_TEXT_CHARS)
}

fn truncate_thread_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value.chars().take(max_chars).collect::<String>();
    out.push_str("\n\n[details truncated]");
    out
}

fn thread_change_label(change: am_agents::ChangeKind) -> &'static str {
    match change {
        am_agents::ChangeKind::Created => "Created",
        am_agents::ChangeKind::Modified => "Edited",
        am_agents::ChangeKind::Deleted => "Deleted",
    }
}

fn user_thread_event(
    thread_id: &str,
    turn_id: &str,
    message: &str,
    client_message_id: Option<&str>,
) -> AgentThreadEvent {
    AgentThreadEvent {
        id: new_id(),
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        role: "user".to_string(),
        kind: "user_message".to_string(),
        text: Some(message.to_string()),
        client_message_id: client_message_id.map(str::to_string),
        data: json!({ "client_message_id": client_message_id }),
        ts: now(),
    }
}

fn build_thread_handoff_summary(
    agent: AgentKind,
    status: SessionStatus,
    events: &[AgentThreadEvent],
    diff: &AgentThreadDiff,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "### Handoff {} ({})\n",
        now().to_rfc3339(),
        status_label(status)
    ));
    out.push_str(&format!("- Agent: {}\n", agent.label()));
    out.push_str(&format!("- Status: {}\n", status_label(status)));
    if diff.repos.is_empty() {
        out.push_str("- Changed files: none detected.\n");
    } else {
        out.push_str("- Changed files:\n");
        for repo in &diff.repos {
            for file in repo.files.iter().take(8) {
                out.push_str(&format!(
                    "  - {} / {} ({}, +{}/-{})\n",
                    repo.repo_name, file.path, file.status, file.additions, file.deletions
                ));
            }
        }
    }
    if let Some(limit) = latest_event_text(events, "usage_limit") {
        out.push_str(&format!(
            "- Usage limit: {}\n",
            truncate_text(limit, MAX_EVENT_TEXT_CHARS).replace('\n', " ")
        ));
    }
    if let Some(error) = latest_event_text(events, "error") {
        out.push_str(&format!(
            "- Last error: {}\n",
            truncate_text(error, MAX_EVENT_TEXT_CHARS).replace('\n', " ")
        ));
    }
    out.push('\n');
    match latest_assistant_text(events) {
        Some(text) => {
            out.push_str("Recent assistant output:\n");
            for line in truncate_text(text, MAX_EVENT_TEXT_CHARS).lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        None => out.push_str("Recent assistant output: none captured.\n"),
    }
    out.trim_end().to_string()
}

fn thread_next_actions(status: SessionStatus, events: &[AgentThreadEvent]) -> String {
    match status {
        SessionStatus::Completed => {
            "Review the repo diffs, run relevant validation, then continue if more work remains."
                .to_string()
        }
        SessionStatus::Interrupted => {
            if latest_event_text(events, "usage_limit").is_some() {
                "Continue with the next available agent, or resume this agent after its limit resets."
                    .to_string()
            } else {
                "Resume from the same workspace and context.".to_string()
            }
        }
        SessionStatus::Failed => latest_event_text(events, "error")
            .map(|err| {
                format!(
                    "Investigate the last failure ({}), then resume from this workspace.",
                    truncate_text(err, 300).replace('\n', " ")
                )
            })
            .unwrap_or_else(|| {
                "Inspect the failed turn, fix the blocker, then resume.".to_string()
            }),
    }
}

pub(crate) fn append_thread_progress(existing: &str, entry: &str) -> String {
    let combined = if existing.trim().is_empty() {
        entry.trim().to_string()
    } else {
        format!("{}\n\n{}", existing.trim_end(), entry.trim())
    };
    if combined.len() <= MAX_THREAD_PROGRESS_BYTES {
        return combined;
    }
    let mut start = combined.len() - MAX_THREAD_PROGRESS_BYTES;
    while start < combined.len() && !combined.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "Older handoff entries were trimmed to keep context bounded.\n\n{}",
        combined[start..].trim_start()
    )
}

fn latest_assistant_text(events: &[AgentThreadEvent]) -> Option<&str> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == "assistant_text")
        .and_then(|event| event.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn latest_event_text<'a>(events: &'a [AgentThreadEvent], kind: &str) -> Option<&'a str> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == kind)
        .and_then(|event| event.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    let mut out = String::new();
    let mut chars = value.chars();
    for _ in 0..max_chars {
        match chars.next() {
            Some(ch) => out.push(ch),
            None => return out,
        }
    }
    if chars.next().is_some() {
        out.push_str("\n[truncated]");
    }
    out
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Completed => "completed",
        SessionStatus::Interrupted => "interrupted",
        SessionStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_thread() -> AgentThread {
        let now = chrono::Utc::now();
        AgentThread {
            id: "thread-1".into(),
            project_id: None,
            group_id: None,
            title: "Fix auth".into(),
            status: TaskStatus::Draft,
            active_agent: None,
            preferred_agent: Some(AgentKind::ClaudeCode),
            permission: permission_to_string(PermissionPolicy::WorkspaceWrite),
            execution_backend: ExecutionBackend::Host,
            model: None,
            reasoning: None,
            local_provider: None,
            local_base_url: None,
            model_target: ModelTargetKind::default(),
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
            handoff_state: "local".into(),
            objective: "Fix auth".into(),
            decisions: String::new(),
            progress: String::new(),
            open_questions: String::new(),
            next_actions: String::new(),
            task_budget: TaskBudget::default(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn initial_prompt_keeps_context_for_slash_like_text() {
        let prompt = build_thread_initial_prompt(&test_thread(), "/plan fix auth", false);
        assert!(prompt.starts_with("/plan fix auth\n\nUse the current repository"));
    }

    #[test]
    fn ordinary_initial_prompt_keeps_context_reminder() {
        let prompt = build_thread_initial_prompt(&test_thread(), "fix auth", true);
        assert!(prompt.starts_with("fix auth\n\nBefore making changes"));
    }

    #[test]
    fn direct_initial_prompt_uses_visible_repo_instruction() {
        let prompt = build_thread_initial_prompt(&test_thread(), "fix auth", false);
        assert!(prompt.contains("current repository working tree directly"));
        assert!(!prompt.contains("TASK_CONTEXT.md"));
    }

    #[test]
    fn model_compatibility_blocks_cross_agent_ids() {
        // Claude can't run a Codex model id, and vice versa.
        assert!(!model_compatible_with_agent(
            AgentKind::ClaudeCode,
            "gpt-5.5"
        ));
        assert!(!model_compatible_with_agent(AgentKind::ClaudeCode, "o3"));
        assert!(!model_compatible_with_agent(
            AgentKind::Codex,
            "claude-opus-4-8"
        ));
        assert!(!model_compatible_with_agent(AgentKind::Codex, "sonnet"));
    }

    #[test]
    fn model_compatibility_allows_matching_and_blank() {
        assert!(model_compatible_with_agent(
            AgentKind::ClaudeCode,
            "claude-opus-4-8"
        ));
        assert!(model_compatible_with_agent(AgentKind::Codex, "gpt-5.5"));
        // Blank means "use the CLI default" — always compatible.
        assert!(model_compatible_with_agent(AgentKind::ClaudeCode, ""));
        assert!(model_compatible_with_agent(AgentKind::Codex, "  "));
    }

    #[test]
    fn sandbox_preflight_detects_container_localhost_model_endpoints() {
        let local = am_agents::LocalModelRuntime {
            provider: am_proto::LocalModelProviderKind::Ollama,
            model: "qwen2.5-coder".into(),
            base_url: Some("http://localhost:11434".into()),
            api_token: None,
        };
        assert!(local_model_uses_container_localhost(&local));

        let local = am_agents::LocalModelRuntime {
            base_url: Some("http://host.docker.internal:11434".into()),
            ..local
        };
        assert!(!local_model_uses_container_localhost(&local));
    }
}
