use am_proto::{
    new_id, now, AgentThreadEvent, AppEvent, ClaimedCollaborationAssignment,
    CollaborationApprovalDecision, CollaborationAssignment, CollaborationAssignmentStatus,
    CollaborationChangeSet, CollaborationChangeStatus, CollaborationDevice,
    CollaborationEventInput, CollaborationSnapshot, FinishCollaborationAssignment,
    NewCollaborationAssignment, NewCollaborationChangeSet, RegisterCollaborationDevice,
    ReportCollaborationApproval, SessionState, TaskStatus,
};
use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::{AppCore, CoreError};

const LEASE_SECONDS: i64 = 45;
const MAX_PROMPT_BYTES: usize = 24 * 1024;
const MAX_EVENT_TEXT_BYTES: usize = 512 * 1024;
const MAX_PATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_DATA_BYTES: usize = 256 * 1024;

impl AppCore {
    pub async fn register_collaboration_device(
        &self,
        input: RegisterCollaborationDevice,
    ) -> Result<CollaborationDevice, CoreError> {
        validate_device_input(&input)?;
        let device =
            am_db::repos::collaboration::upsert_device(&self.db.pool, &input, true).await?;
        self.events
            .publish(AppEvent::CollaborationDeviceUpdated(device.clone()));
        self.activity(
            None,
            None,
            "collaboration.device_online",
            json!({ "device_id": device.id, "name": device.name }),
        )
        .await?;
        Ok(device)
    }

    pub async fn heartbeat_collaboration_device(
        &self,
        input: RegisterCollaborationDevice,
    ) -> Result<CollaborationDevice, CoreError> {
        validate_device_input(&input)?;
        self.expire_collaboration_leases().await?;
        let device = am_db::repos::collaboration::heartbeat_device(&self.db.pool, &input).await?;
        self.events
            .publish(AppEvent::CollaborationDeviceUpdated(device.clone()));
        Ok(device)
    }

    pub async fn list_collaboration_devices(&self) -> Result<Vec<CollaborationDevice>, CoreError> {
        self.expire_collaboration_leases().await?;
        Ok(am_db::repos::collaboration::list_devices(&self.db.pool).await?)
    }

    pub async fn revoke_collaboration_device(&self, device_id: &str) -> Result<(), CoreError> {
        let device = am_db::repos::collaboration::get_device(&self.db.pool, device_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        am_db::repos::collaboration::revoke_device(&self.db.pool, device_id).await?;
        let revoked = am_db::repos::collaboration::get_device(&self.db.pool, device_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        self.events
            .publish(AppEvent::CollaborationDeviceUpdated(revoked));
        self.activity(
            None,
            None,
            "collaboration.device_revoked",
            json!({ "device_id": device_id, "name": device.name }),
        )
        .await?;
        Ok(())
    }

    pub async fn collaboration_snapshot(
        &self,
        thread_id: Option<&str>,
        include_patches: bool,
    ) -> Result<CollaborationSnapshot, CoreError> {
        self.expire_collaboration_leases().await?;
        let mut change_sets =
            am_db::repos::collaboration::list_change_sets(&self.db.pool, thread_id).await?;
        if !include_patches {
            for change in &mut change_sets {
                change.patch.clear();
            }
        }
        Ok(CollaborationSnapshot {
            devices: am_db::repos::collaboration::list_devices(&self.db.pool).await?,
            assignments: am_db::repos::collaboration::list_assignments(&self.db.pool, None, false)
                .await?
                .into_iter()
                .filter(|assignment| thread_id.is_none_or(|id| assignment.thread_id == id))
                .collect(),
            change_sets,
            server_time: now(),
        })
    }

    pub async fn create_collaboration_assignment(
        &self,
        input: NewCollaborationAssignment,
    ) -> Result<CollaborationAssignment, CoreError> {
        self.create_collaboration_assignment_inner(input, true)
            .await
    }

    pub async fn retry_collaboration_assignment(
        &self,
        assignment_id: &str,
    ) -> Result<CollaborationAssignment, CoreError> {
        let previous = am_db::repos::collaboration::get_assignment(&self.db.pool, assignment_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if !matches!(
            previous.status,
            CollaborationAssignmentStatus::Failed | CollaborationAssignmentStatus::LeaseExpired
        ) {
            return Err(CoreError::Other(
                "Only failed or disconnected device work can be retried.".into(),
            ));
        }
        self.create_collaboration_assignment_inner(
            NewCollaborationAssignment {
                thread_id: previous.thread_id,
                device_id: previous.device_id,
                agent: previous.agent,
                permission: previous.permission,
                execution_backend: previous.execution_backend,
                message: None,
                client_message_id: None,
            },
            false,
        )
        .await
    }

    async fn create_collaboration_assignment_inner(
        &self,
        input: NewCollaborationAssignment,
        record_user_message: bool,
    ) -> Result<CollaborationAssignment, CoreError> {
        self.expire_collaboration_leases().await?;
        if self.sessions.is_active(&input.thread_id).await {
            return Err(CoreError::Other(
                "Stop the local run before assigning this session to another device.".into(),
            ));
        }
        validate_permission(&input.permission)?;
        if input.execution_backend == am_proto::ExecutionBackend::Cloud {
            return Err(CoreError::Other(
                "Provider cloud runs are coordinated separately from device runs.".into(),
            ));
        }

        let device = am_db::repos::collaboration::get_device(&self.db.pool, &input.device_id)
            .await?
            .ok_or_else(|| CoreError::Other("The selected device is no longer paired.".into()))?;
        if device.revoked_at.is_some() {
            return Err(CoreError::Other(
                "The selected device has been revoked. Pair it again before assigning work.".into(),
            ));
        }
        if now() - device.last_seen_at > Duration::seconds(LEASE_SECONDS) {
            return Err(CoreError::Other(format!(
                "{} is offline. Open Perpetual on that device and try again.",
                device.name
            )));
        }
        let capable = device.capabilities.iter().any(|capability| {
            capability.agent == input.agent && capability.installed && capability.authenticated
        });
        if !capable {
            return Err(CoreError::Other(format!(
                "{} is not ready on {}.",
                input.agent.label(),
                device.name
            )));
        }

        let mut thread = am_db::repos::agent_thread::get(&self.db.pool, &input.thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let repo_ids = if input.permission == "read_only" {
            Vec::new()
        } else {
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &thread.id)
                .await?
                .into_iter()
                .map(|binding| binding.repo_id)
                .collect::<Vec<_>>()
        };
        let locked =
            am_db::repos::collaboration::locked_repo_names(&self.db.pool, &repo_ids).await?;
        if !locked.is_empty() {
            return Err(CoreError::Other(format!(
                "Another shared agent is already editing {}. Finish or review that work first.",
                locked.join(", ")
            )));
        }
        let prompt = self
            .build_collaboration_prompt(&thread, input.message.as_deref(), &device.name)
            .await?;
        let turn = am_db::repos::agent_turn::create(
            &self.db.pool,
            &thread.id,
            input.agent,
            &input.permission,
            input.execution_backend,
            None,
            thread.model.as_deref(),
            thread.reasoning.as_deref(),
            thread.local_provider,
            thread.local_base_url.as_deref(),
            thread.model_target,
            thread.compute_lease_id.as_deref(),
            thread.compute_provider,
            thread.estimated_compute_cost_usd,
            thread.fallback_model_target,
            None,
            None,
        )
        .await?;

        let user_text = input
            .message
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| thread.objective.trim());
        if record_user_message && !user_text.is_empty() {
            let event = AgentThreadEvent {
                id: new_id(),
                thread_id: thread.id.clone(),
                turn_id: turn.id.clone(),
                role: "user".into(),
                kind: "user_message".into(),
                text: Some(user_text.to_string()),
                client_message_id: input.client_message_id.clone(),
                data: json!({
                    "remote_device_id": device.id,
                    "remote_device_name": device.name,
                }),
                ts: now(),
            };
            am_db::repos::agent_thread_message::insert(&self.db.pool, &event).await?;
            self.events.publish(AppEvent::AgentThreadEvent(event));
        }

        let assignment = CollaborationAssignment {
            id: new_id(),
            thread_id: thread.id.clone(),
            turn_id: turn.id,
            device_id: device.id.clone(),
            device_name: device.name.clone(),
            agent: input.agent,
            permission: input.permission.clone(),
            execution_backend: input.execution_backend,
            prompt,
            status: CollaborationAssignmentStatus::Queued,
            lease_expires_at: None,
            created_at: now(),
            started_at: None,
            finished_at: None,
            error: None,
        };
        if let Err(error) =
            am_db::repos::collaboration::insert_assignment(&self.db.pool, &assignment, &repo_ids)
                .await
        {
            if error
                .to_string()
                .contains("collaboration_repo_leases.repo_id")
            {
                return Err(CoreError::Other(
                    "Another shared agent claimed one of these repositories. Try again after its work is reviewed."
                        .into(),
                ));
            }
            return Err(error.into());
        }

        thread.status = TaskStatus::Queued;
        thread.active_agent = Some(input.agent);
        thread.permission = input.permission;
        thread.execution_backend = input.execution_backend;
        if thread.preferred_agent.is_none() {
            thread.preferred_agent = Some(input.agent);
        }
        let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
        let project_id = thread.project_id.clone();
        self.events
            .publish(AppEvent::AgentThreadUpdated(thread.clone()));
        self.events
            .publish(AppEvent::CollaborationAssignmentUpdated(assignment.clone()));
        self.activity(
            project_id,
            None,
            "collaboration.assignment_queued",
            json!({
                "assignment_id": assignment.id,
                "thread_id": assignment.thread_id,
                "device_id": assignment.device_id,
                "device_name": assignment.device_name,
                "agent": assignment.agent.as_str(),
            }),
        )
        .await?;
        Ok(assignment)
    }

    pub async fn list_collaboration_assignments(
        &self,
        device_id: Option<&str>,
        active_only: bool,
    ) -> Result<Vec<CollaborationAssignment>, CoreError> {
        self.expire_collaboration_leases().await?;
        Ok(
            am_db::repos::collaboration::list_assignments(&self.db.pool, device_id, active_only)
                .await?,
        )
    }

    pub async fn claim_collaboration_assignment(
        &self,
        assignment_id: &str,
        device_id: &str,
    ) -> Result<ClaimedCollaborationAssignment, CoreError> {
        self.expire_collaboration_leases().await?;
        let lease_token = format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""));
        let token_hash = hash_secret(&lease_token);
        let expires = Utc::now() + Duration::seconds(LEASE_SECONDS);
        let assignment = am_db::repos::collaboration::claim_assignment(
            &self.db.pool,
            assignment_id,
            device_id,
            &token_hash,
            expires,
        )
        .await?
        .ok_or_else(|| {
            CoreError::Other(
                "This assignment was already claimed, cancelled, or the device was revoked.".into(),
            )
        })?;

        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
        {
            thread.status = TaskStatus::Running;
            let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
            self.events.publish(AppEvent::AgentThreadUpdated(thread));
        }
        self.events
            .publish(AppEvent::CollaborationAssignmentUpdated(assignment.clone()));
        Ok(ClaimedCollaborationAssignment {
            assignment,
            lease_token,
        })
    }

    pub async fn renew_collaboration_lease(
        &self,
        assignment_id: &str,
        lease_token: &str,
    ) -> Result<CollaborationAssignment, CoreError> {
        let expires = Utc::now() + Duration::seconds(LEASE_SECONDS);
        let assignment = am_db::repos::collaboration::renew_lease(
            &self.db.pool,
            assignment_id,
            &hash_secret(lease_token),
            expires,
        )
        .await?
        .ok_or_else(stale_lease_error)?;
        Ok(assignment)
    }

    pub async fn report_collaboration_event(
        &self,
        input: CollaborationEventInput,
    ) -> Result<(), CoreError> {
        let assignment = self
            .validate_collaboration_lease(&input.assignment_id, &input.lease_token)
            .await?;
        if input.event_id.trim().is_empty() || input.event_id.len() > 256 {
            return Err(CoreError::Other("Invalid remote event id.".into()));
        }
        if input.role.len() > 32 || input.kind.len() > 96 {
            return Err(CoreError::Other("Invalid remote event envelope.".into()));
        }
        let text = input
            .text
            .map(|value| truncate_utf8(&value, MAX_EVENT_TEXT_BYTES));
        let data = if serde_json::to_vec(&input.data)
            .map(|value| value.len())
            .unwrap_or(usize::MAX)
            <= MAX_EVENT_DATA_BYTES
        {
            input.data
        } else {
            json!({ "truncated": true, "reason": "remote event data exceeded limit" })
        };
        let event = AgentThreadEvent {
            id: format!("collab:{}:{}", assignment.id, input.event_id),
            thread_id: assignment.thread_id,
            turn_id: assignment.turn_id,
            role: input.role,
            kind: input.kind,
            text,
            client_message_id: input.client_message_id,
            data: merge_remote_metadata(data, &assignment.device_id, &assignment.device_name),
            ts: input.ts,
        };
        am_db::repos::agent_thread_message::upsert(&self.db.pool, &event).await?;
        self.events.publish(AppEvent::AgentThreadEvent(event));
        Ok(())
    }

    pub async fn report_collaboration_approval(
        &self,
        input: ReportCollaborationApproval,
    ) -> Result<am_proto::ApprovalRequest, CoreError> {
        let assignment = self
            .validate_collaboration_lease(&input.assignment_id, &input.lease_token)
            .await?;
        if input.approval.id.trim().is_empty() || input.approval.id.len() > 256 {
            return Err(CoreError::Other("Invalid remote approval id.".into()));
        }
        if serde_json::to_vec(&input.approval)
            .map(|value| value.len())
            .unwrap_or(usize::MAX)
            > MAX_EVENT_DATA_BYTES
        {
            return Err(CoreError::Other(
                "Remote approval details exceeded the safety limit.".into(),
            ));
        }
        let thread = am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let local_approval_id = input.approval.id.clone();
        let id = format!(
            "collab:{}:{}",
            assignment.id,
            &hash_bytes(local_approval_id.as_bytes())[..16]
        );
        let mut approval = input.approval;
        approval.id = id.clone();
        approval.project_id = thread.project_id.clone();
        approval.work_node_id = None;
        approval.task_id = None;
        approval.thread_id = Some(assignment.thread_id.clone());
        approval.session_id = Some(assignment.turn_id.clone());
        let approval = am_db::repos::collaboration::insert_approval(
            &self.db.pool,
            &id,
            &assignment.id,
            &local_approval_id,
            &approval,
        )
        .await?;
        self.events
            .publish(AppEvent::ApprovalRequested(approval.clone()));
        Ok(approval)
    }

    pub async fn list_collaboration_approval_decisions(
        &self,
        assignment_id: &str,
        lease_token: &str,
    ) -> Result<Vec<CollaborationApprovalDecision>, CoreError> {
        self.validate_collaboration_lease(assignment_id, lease_token)
            .await?;
        Ok(
            am_db::repos::collaboration::list_approval_decisions(&self.db.pool, assignment_id)
                .await?,
        )
    }

    pub async fn acknowledge_collaboration_approval_decision(
        &self,
        assignment_id: &str,
        lease_token: &str,
        approval_id: &str,
    ) -> Result<(), CoreError> {
        self.validate_collaboration_lease(assignment_id, lease_token)
            .await?;
        if !am_db::repos::collaboration::acknowledge_approval_decision(
            &self.db.pool,
            assignment_id,
            approval_id,
        )
        .await?
        {
            return Err(CoreError::NotFound);
        }
        Ok(())
    }

    pub async fn report_collaboration_change_set(
        &self,
        input: NewCollaborationChangeSet,
    ) -> Result<CollaborationChangeSet, CoreError> {
        let assignment = self
            .validate_collaboration_lease(&input.assignment_id, &input.lease_token)
            .await?;
        if input.patch.len() > MAX_PATCH_BYTES {
            return Err(CoreError::Other(format!(
                "Remote patch is larger than {} MiB. Commit and push the branch instead.",
                MAX_PATCH_BYTES / 1024 / 1024
            )));
        }
        let repo = am_db::repos::repo::get(&self.db.pool, &input.repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let linked =
            am_db::repos::agent_thread_repo::list_for_thread(&self.db.pool, &assignment.thread_id)
                .await?
                .iter()
                .any(|binding| binding.repo_id == repo.id);
        if !linked {
            return Err(CoreError::Other(
                "The reported repository is not attached to this session.".into(),
            ));
        }
        let patch_sha256 = hash_bytes(input.patch.as_bytes());
        let change = CollaborationChangeSet {
            id: new_id(),
            assignment_id: assignment.id,
            thread_id: assignment.thread_id,
            device_id: assignment.device_id,
            repo_id: repo.id,
            repo_name: repo.name,
            base_ref: input.base_ref,
            files: input.files.into_iter().take(2_000).collect(),
            patch: input.patch,
            patch_sha256,
            status: CollaborationChangeStatus::Pending,
            conflict_files: Vec::new(),
            created_at: now(),
            applied_at: None,
        };
        let change = am_db::repos::collaboration::insert_change_set(&self.db.pool, &change).await?;
        self.events
            .publish(AppEvent::CollaborationChangeSetUpdated(change.clone()));
        Ok(change)
    }

    pub async fn finish_collaboration_assignment(
        &self,
        input: FinishCollaborationAssignment,
    ) -> Result<CollaborationAssignment, CoreError> {
        let status = match input.state {
            SessionState::Completed => CollaborationAssignmentStatus::Review,
            SessionState::Interrupted => CollaborationAssignmentStatus::Failed,
            SessionState::Failed => CollaborationAssignmentStatus::Failed,
            SessionState::Running => {
                return Err(CoreError::Other(
                    "A running assignment cannot be reported as finished.".into(),
                ))
            }
        };
        let mut assignment = am_db::repos::collaboration::finish_assignment(
            &self.db.pool,
            &input.assignment_id,
            &hash_secret(&input.lease_token),
            status,
            input.error.as_deref(),
        )
        .await?
        .ok_or_else(stale_lease_error)?;
        if input.state == SessionState::Completed {
            if let Some(completed) = am_db::repos::collaboration::complete_assignment_review(
                &self.db.pool,
                &assignment.id,
            )
            .await?
            {
                assignment = completed;
            }
        }
        am_db::repos::agent_turn::finish(&self.db.pool, &assignment.turn_id, input.state).await?;
        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
        {
            thread.status = match input.state {
                SessionState::Completed
                    if assignment.status == CollaborationAssignmentStatus::Completed =>
                {
                    TaskStatus::Done
                }
                SessionState::Completed => TaskStatus::Review,
                SessionState::Interrupted => TaskStatus::Paused,
                SessionState::Failed => TaskStatus::Failed,
                SessionState::Running => unreachable!("validated above"),
            };
            let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
            self.events.publish(AppEvent::AgentThreadUpdated(thread));
        }
        self.events
            .publish(AppEvent::CollaborationAssignmentUpdated(assignment.clone()));
        Ok(assignment)
    }

    /// Apply a worker's isolated patch to the coordinator checkout. The normal
    /// path never writes over locally dirty files. `overwrite` is an explicit
    /// remote-wins action and retains a file-level backup under app data.
    pub async fn apply_collaboration_change_set(
        &self,
        change_set_id: &str,
        overwrite: bool,
    ) -> Result<CollaborationChangeSet, CoreError> {
        let change = am_db::repos::collaboration::get_change_set(&self.db.pool, change_set_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if !matches!(
            change.status,
            CollaborationChangeStatus::Pending | CollaborationChangeStatus::Conflict
        ) {
            return Err(CoreError::Other(
                "This returned change set has already been resolved.".into(),
            ));
        }
        if hash_bytes(change.patch.as_bytes()) != change.patch_sha256 {
            return Err(CoreError::Other(
                "The stored patch failed its integrity check and was not applied.".into(),
            ));
        }
        let repo = am_db::repos::repo::get(&self.db.pool, &change.repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let target = repo.local_path.map(PathBuf::from).ok_or_else(|| {
            CoreError::Other("Repository has no local checkout on the coordinator.".into())
        })?;
        let paths = change
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let target_for_dirty = target.clone();
        let dirty =
            tokio::task::spawn_blocking(move || am_vcs::dirty_paths(&target_for_dirty, &paths))
                .await
                .map_err(|error| CoreError::Other(error.to_string()))?
                .map_err(|error| CoreError::Other(error.to_string()))?;
        if !dirty.is_empty() && !overwrite {
            let conflict = am_db::repos::collaboration::update_change_status(
                &self.db.pool,
                &change.id,
                CollaborationChangeStatus::Conflict,
                &dirty,
            )
            .await?
            .ok_or(CoreError::NotFound)?;
            self.events
                .publish(AppEvent::CollaborationChangeSetUpdated(conflict.clone()));
            return Ok(conflict);
        }

        let patch = change.patch.clone();
        let status = if overwrite {
            let base_ref = change.base_ref.clone().ok_or_else(|| {
                CoreError::Other(
                    "This change set has no base revision, so a safe overwrite cannot be materialized."
                        .into(),
                )
            })?;
            let scratch = self
                .data_dir
                .join("collaboration-overwrite")
                .join(&change.id);
            let backup = self.data_dir.join("collaboration-backups").join(&change.id);
            let overwrite_target = target.clone();
            let overwrite_paths = change
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            let backup_for_activity = backup.clone();
            tokio::task::spawn_blocking(move || {
                am_vcs::overwrite_patch_paths(
                    &overwrite_target,
                    &base_ref,
                    &patch,
                    &overwrite_paths,
                    &scratch,
                    &backup,
                )
            })
            .await
            .map_err(|error| CoreError::Other(error.to_string()))?
            .map_err(|error| {
                CoreError::Other(format!("Remote changes were not applied: {error}"))
            })?;
            self.activity(
                None,
                None,
                "collaboration.changes_overwrote_local",
                json!({
                    "change_set_id": change.id,
                    "repo_id": change.repo_id,
                    "backup_path": backup_for_activity.to_string_lossy(),
                }),
            )
            .await?;
            CollaborationChangeStatus::AppliedWithOverwrite
        } else {
            let check_target = target.clone();
            let check_patch = patch.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                am_vcs::check_patch_applies(&check_target, &check_patch)
            })
            .await
            .map_err(|error| CoreError::Other(error.to_string()))?
            {
                let conflict_files = change
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let conflict = am_db::repos::collaboration::update_change_status(
                    &self.db.pool,
                    &change.id,
                    CollaborationChangeStatus::Conflict,
                    &conflict_files,
                )
                .await?
                .ok_or(CoreError::NotFound)?;
                self.events
                    .publish(AppEvent::CollaborationChangeSetUpdated(conflict.clone()));
                self.activity(
                    None,
                    None,
                    "collaboration.change_conflict",
                    json!({ "change_set_id": change.id, "reason": error.to_string() }),
                )
                .await?;
                return Ok(conflict);
            }
            let apply_target = target;
            tokio::task::spawn_blocking(move || am_vcs::apply_patch_to_repo(&apply_target, &patch))
                .await
                .map_err(|error| CoreError::Other(error.to_string()))?
                .map_err(|error| {
                    CoreError::Other(format!("Remote changes were not applied: {error}"))
                })?;
            CollaborationChangeStatus::Applied
        };

        let applied = am_db::repos::collaboration::update_change_status(
            &self.db.pool,
            &change.id,
            status,
            &[],
        )
        .await?
        .ok_or(CoreError::NotFound)?;
        self.events
            .publish(AppEvent::CollaborationChangeSetUpdated(applied.clone()));
        self.finish_collaboration_review_if_settled(&applied.assignment_id)
            .await?;
        Ok(applied)
    }

    pub async fn reject_collaboration_change_set(
        &self,
        change_set_id: &str,
    ) -> Result<CollaborationChangeSet, CoreError> {
        let current = am_db::repos::collaboration::get_change_set(&self.db.pool, change_set_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if !matches!(
            current.status,
            CollaborationChangeStatus::Pending | CollaborationChangeStatus::Conflict
        ) {
            return Err(CoreError::Other(
                "This returned change set has already been resolved.".into(),
            ));
        }
        let rejected = am_db::repos::collaboration::update_change_status(
            &self.db.pool,
            change_set_id,
            CollaborationChangeStatus::Rejected,
            &[],
        )
        .await?
        .ok_or(CoreError::NotFound)?;
        self.events
            .publish(AppEvent::CollaborationChangeSetUpdated(rejected.clone()));
        self.finish_collaboration_review_if_settled(&rejected.assignment_id)
            .await?;
        Ok(rejected)
    }

    pub async fn cancel_collaboration_assignment(
        &self,
        assignment_id: &str,
    ) -> Result<CollaborationAssignment, CoreError> {
        let assignment =
            am_db::repos::collaboration::cancel_assignment(&self.db.pool, assignment_id)
                .await?
                .ok_or(CoreError::NotFound)?;
        am_db::repos::agent_turn::finish(
            &self.db.pool,
            &assignment.turn_id,
            SessionState::Interrupted,
        )
        .await?;
        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
        {
            thread.status = TaskStatus::Paused;
            let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
            self.events.publish(AppEvent::AgentThreadUpdated(thread));
        }
        self.events
            .publish(AppEvent::CollaborationAssignmentUpdated(assignment.clone()));
        Ok(assignment)
    }

    pub(crate) async fn has_active_collaboration_assignment(
        &self,
        thread_id: &str,
    ) -> Result<bool, CoreError> {
        self.expire_collaboration_leases().await?;
        Ok(
            am_db::repos::collaboration::list_assignments(&self.db.pool, None, true)
                .await?
                .iter()
                .any(|assignment| assignment.thread_id == thread_id),
        )
    }

    async fn validate_collaboration_lease(
        &self,
        assignment_id: &str,
        lease_token: &str,
    ) -> Result<CollaborationAssignment, CoreError> {
        am_db::repos::collaboration::validate_lease(
            &self.db.pool,
            assignment_id,
            &hash_secret(lease_token),
        )
        .await?
        .ok_or_else(stale_lease_error)
    }

    async fn finish_collaboration_review_if_settled(
        &self,
        assignment_id: &str,
    ) -> Result<(), CoreError> {
        let Some(assignment) =
            am_db::repos::collaboration::complete_assignment_review(&self.db.pool, assignment_id)
                .await?
        else {
            return Ok(());
        };
        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
        {
            thread.status = TaskStatus::Done;
            let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
            self.events.publish(AppEvent::AgentThreadUpdated(thread));
        }
        self.events
            .publish(AppEvent::CollaborationAssignmentUpdated(assignment));
        Ok(())
    }

    async fn expire_collaboration_leases(&self) -> Result<(), CoreError> {
        let expired = am_db::repos::collaboration::expire_stale_assignments(&self.db.pool).await?;
        for assignment in expired {
            am_db::repos::agent_turn::finish(
                &self.db.pool,
                &assignment.turn_id,
                SessionState::Interrupted,
            )
            .await?;
            if let Some(mut thread) =
                am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
            {
                thread.status = TaskStatus::WaitingForNetwork;
                let thread = am_db::repos::agent_thread::save(&self.db.pool, &thread).await?;
                self.events.publish(AppEvent::AgentThreadUpdated(thread));
            }
            self.events
                .publish(AppEvent::CollaborationAssignmentUpdated(assignment));
        }
        Ok(())
    }

    async fn build_collaboration_prompt(
        &self,
        thread: &am_proto::AgentThread,
        message: Option<&str>,
        device_name: &str,
    ) -> Result<String, CoreError> {
        let events =
            am_db::repos::agent_thread_message::list_for_thread(&self.db.pool, &thread.id).await?;
        let recent = events
            .iter()
            .rev()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "assistant_text" | "user_message" | "error"
                )
            })
            .take(8)
            .collect::<Vec<_>>();
        let mut prompt = format!(
            "# Perpetual multi-device handoff\n\nYou are running on {device_name}. Another Perpetual instance is the coordinator. Work only in the isolated workspace provided locally; Perpetual will reconcile your patch.\n\n## Objective\n{}\n\n## Decisions\n{}\n\n## Progress\n{}\n\n## Open questions\n{}\n\n## Next actions\n{}\n",
            nonempty(&thread.objective),
            nonempty(&thread.decisions),
            nonempty(&thread.progress),
            nonempty(&thread.open_questions),
            nonempty(&thread.next_actions),
        );
        if let Some(message) = message.map(str::trim).filter(|value| !value.is_empty()) {
            prompt.push_str("\n## Current user request\n");
            prompt.push_str(message);
            prompt.push('\n');
        }
        if !recent.is_empty() {
            prompt.push_str("\n## Recent shared activity\n");
            for event in recent.into_iter().rev() {
                let label = if event.role == "user" {
                    "User"
                } else {
                    "Agent"
                };
                if let Some(text) = event.text.as_deref() {
                    prompt.push_str(&format!("\n{label}: {}\n", truncate_utf8(text, 1_600)));
                }
            }
        }
        prompt.push_str("\n## Collaboration rules\n- Inspect the workspace before editing; another device may have advanced the task.\n- Keep changes focused and report concrete progress and changed files.\n- Do not undo peer changes unless the current request requires it; explain intentional overwrites.\n- Do not copy credentials or provider account data into the transcript.\n");
        Ok(truncate_utf8(&prompt, MAX_PROMPT_BYTES))
    }
}

fn validate_device_input(input: &RegisterCollaborationDevice) -> Result<(), CoreError> {
    if input.id.trim().is_empty() || input.id.len() > 128 {
        return Err(CoreError::Other("Invalid device id.".into()));
    }
    if input.name.trim().is_empty() || input.name.len() > 80 {
        return Err(CoreError::Other(
            "Device names must be between 1 and 80 characters.".into(),
        ));
    }
    if input.hostname.len() > 255 || input.platform.len() > 80 || input.extension_version.len() > 40
    {
        return Err(CoreError::Other("Invalid device metadata.".into()));
    }
    if input.capabilities.len() > 16 {
        return Err(CoreError::Other("Too many device capabilities.".into()));
    }
    Ok(())
}

fn validate_permission(permission: &str) -> Result<(), CoreError> {
    if matches!(
        permission,
        "read_only" | "workspace_write" | "ask" | "autonomous"
    ) {
        Ok(())
    } else {
        Err(CoreError::Other("Unsupported permission mode.".into()))
    }
}

fn hash_secret(secret: &str) -> String {
    hash_bytes(secret.as_bytes())
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stale_lease_error() -> CoreError {
    CoreError::Other(
        "The device lease expired or was replaced. Its late output was kept isolated and was not accepted."
            .into(),
    )
}

fn merge_remote_metadata(
    data: serde_json::Value,
    device_id: &str,
    device_name: &str,
) -> serde_json::Value {
    let mut map = match data {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("provider_data".into(), other);
            map
        }
    };
    map.insert("remote_device_id".into(), json!(device_id));
    map.insert("remote_device_name".into(), json!(device_name));
    serde_json::Value::Object(map)
}

fn nonempty(value: &str) -> &str {
    if value.trim().is_empty() {
        "None recorded."
    } else {
        value.trim()
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(24);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::{
        AgentKind, ApprovalDecision, ApprovalKind, ApprovalRequest, CollaborationAgentCapability,
        ExecutionBackend, NewAgentThread, NewLocalRepo, NewProject,
    };
    use std::process::Command;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let value = "hello 🤖".repeat(100);
        let truncated = truncate_utf8(&value, 80);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.contains("[truncated]"));
    }

    #[test]
    fn secret_hashes_are_stable_without_storing_the_secret() {
        let hash = hash_secret("lease-secret");
        assert_eq!(hash, hash_secret("lease-secret"));
        assert_ne!(hash, hash_secret("other"));
        assert!(!hash.contains("lease-secret"));
    }

    #[tokio::test]
    async fn remote_turn_lifecycle_is_fenced_and_persisted() {
        let dir = std::env::temp_dir().join(format!("am-collaboration-test-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = AppCore::new(&dir).await.unwrap();
        let project = core
            .create_project(NewProject {
                name: "Shared".into(),
                description: None,
            })
            .await
            .unwrap();
        let thread = core
            .create_agent_thread(NewAgentThread {
                project_id: Some(project.id),
                title: "Remote task".into(),
                objective: Some("Implement it".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        core.register_collaboration_device(RegisterCollaborationDevice {
            id: "device-1".into(),
            name: "Laptop".into(),
            hostname: "laptop.local".into(),
            platform: "darwin-arm64".into(),
            extension_version: "0.2.2".into(),
            capabilities: vec![CollaborationAgentCapability {
                agent: AgentKind::Codex,
                installed: true,
                authenticated: true,
                version: Some("test".into()),
            }],
        })
        .await
        .unwrap();

        let queued = core
            .create_collaboration_assignment(NewCollaborationAssignment {
                thread_id: thread.id.clone(),
                device_id: "device-1".into(),
                agent: AgentKind::Codex,
                permission: "workspace_write".into(),
                execution_backend: ExecutionBackend::Host,
                message: Some("Start on the parser".into()),
                client_message_id: Some("client-1".into()),
            })
            .await
            .unwrap();
        assert_eq!(queued.status, CollaborationAssignmentStatus::Queued);

        let claimed = core
            .claim_collaboration_assignment(&queued.id, "device-1")
            .await
            .unwrap();
        assert_eq!(
            claimed.assignment.status,
            CollaborationAssignmentStatus::Running
        );
        core.report_collaboration_event(CollaborationEventInput {
            assignment_id: queued.id.clone(),
            lease_token: claimed.lease_token.clone(),
            event_id: "assistant-1".into(),
            role: "assistant".into(),
            kind: "assistant_text".into(),
            text: Some("Working on it".into()),
            client_message_id: None,
            data: serde_json::Value::Null,
            ts: now(),
        })
        .await
        .unwrap();
        let relayed = core
            .report_collaboration_approval(ReportCollaborationApproval {
                assignment_id: queued.id.clone(),
                lease_token: claimed.lease_token.clone(),
                approval: ApprovalRequest {
                    id: "local-approval".into(),
                    agent: AgentKind::Codex,
                    project_id: None,
                    work_node_id: None,
                    task_id: None,
                    thread_id: Some("local-mirror".into()),
                    session_id: Some("local-turn".into()),
                    kind: ApprovalKind::Command,
                    tool_name: "shell".into(),
                    command: Some(vec!["cargo".into(), "check".into()]),
                    cwd: Some("/managed/worktree".into()),
                    input: serde_json::Value::Null,
                    reason: Some("Verify the change".into()),
                    created_at: now(),
                },
            })
            .await
            .unwrap();
        assert_eq!(relayed.thread_id.as_deref(), Some(thread.id.as_str()));
        assert!(core
            .list_pending_approvals()
            .await
            .iter()
            .any(|approval| approval.id == relayed.id));
        core.resolve_approval(&relayed.id, ApprovalDecision::Allow)
            .await
            .unwrap();
        let decisions = core
            .list_collaboration_approval_decisions(&queued.id, &claimed.lease_token)
            .await
            .unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].local_approval_id, "local-approval");
        core.acknowledge_collaboration_approval_decision(
            &queued.id,
            &claimed.lease_token,
            &decisions[0].id,
        )
        .await
        .unwrap();
        assert!(core
            .list_collaboration_approval_decisions(&queued.id, &claimed.lease_token)
            .await
            .unwrap()
            .is_empty());
        let finished = core
            .finish_collaboration_assignment(FinishCollaborationAssignment {
                assignment_id: queued.id.clone(),
                lease_token: claimed.lease_token.clone(),
                state: SessionState::Failed,
                error: Some("matching clone missing".into()),
            })
            .await
            .unwrap();
        assert_eq!(finished.status, CollaborationAssignmentStatus::Failed);

        let user_messages_before_retry = core
            .list_thread_events(&thread.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.role == "user")
            .count();
        let retry = core
            .retry_collaboration_assignment(&queued.id)
            .await
            .unwrap();
        assert_ne!(retry.id, queued.id);
        assert_eq!(retry.status, CollaborationAssignmentStatus::Queued);
        let user_messages_after_retry = core
            .list_thread_events(&thread.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.role == "user")
            .count();
        assert_eq!(user_messages_after_retry, user_messages_before_retry);

        let events = core.list_thread_events(&thread.id).await.unwrap();
        let remote = events
            .iter()
            .find(|event| event.text.as_deref() == Some("Working on it"))
            .unwrap();
        assert_eq!(
            remote
                .data
                .get("remote_device_name")
                .and_then(|value| value.as_str()),
            Some("Laptop")
        );

        let late = core
            .report_collaboration_event(CollaborationEventInput {
                assignment_id: queued.id.clone(),
                lease_token: claimed.lease_token.clone(),
                event_id: "late".into(),
                role: "assistant".into(),
                kind: "assistant_text".into(),
                text: Some("too late".into()),
                client_message_id: None,
                data: serde_json::Value::Null,
                ts: now(),
            })
            .await;
        assert!(late.is_err(), "finished leases must fence late output");

        let retry_claim = core
            .claim_collaboration_assignment(&retry.id, "device-1")
            .await
            .unwrap();
        let completed = core
            .finish_collaboration_assignment(FinishCollaborationAssignment {
                assignment_id: retry.id,
                lease_token: retry_claim.lease_token,
                state: SessionState::Completed,
                error: None,
            })
            .await
            .unwrap();
        assert_eq!(completed.status, CollaborationAssignmentStatus::Completed);

        core.shutdown().await;
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn remote_changes_are_reviewed_and_explicit_overwrite_is_recoverable() {
        let dir = std::env::temp_dir().join(format!("am-collaboration-apply-{}", new_id()));
        let repo_path = dir.join("repo");
        let producer = dir.join("producer");
        std::fs::create_dir_all(&repo_path).unwrap();
        git(&repo_path, &["init"]);
        git(&repo_path, &["config", "user.name", "Test"]);
        git(&repo_path, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo_path.join("file.txt"), "base\n").unwrap();
        git(&repo_path, &["add", "."]);
        git(&repo_path, &["commit", "-m", "base"]);
        let base = am_vcs::head_sha(&repo_path).unwrap();

        let core = AppCore::new(&dir.join("data")).await.unwrap();
        let project = core
            .create_project(NewProject {
                name: "Shared".into(),
                description: None,
            })
            .await
            .unwrap();
        let repo = core
            .connect_local_repo(NewLocalRepo {
                project_id: project.id.clone(),
                path: repo_path.to_string_lossy().to_string(),
            })
            .await
            .unwrap();
        let thread = core
            .create_agent_thread(NewAgentThread {
                project_id: Some(project.id),
                title: "Remote edit".into(),
                objective: Some("Edit the file".into()),
                repo_ids: vec![repo.id.clone()],
                ..Default::default()
            })
            .await
            .unwrap();
        core.register_collaboration_device(RegisterCollaborationDevice {
            id: "device-edit".into(),
            name: "Laptop".into(),
            hostname: "laptop.local".into(),
            platform: "darwin-arm64".into(),
            extension_version: "test".into(),
            capabilities: vec![CollaborationAgentCapability {
                agent: AgentKind::Codex,
                installed: true,
                authenticated: true,
                version: None,
            }],
        })
        .await
        .unwrap();
        let assignment = core
            .create_collaboration_assignment(NewCollaborationAssignment {
                thread_id: thread.id.clone(),
                device_id: "device-edit".into(),
                agent: AgentKind::Codex,
                permission: "workspace_write".into(),
                execution_backend: ExecutionBackend::Host,
                message: None,
                client_message_id: None,
            })
            .await
            .unwrap();
        let second_thread = core
            .create_agent_thread(NewAgentThread {
                project_id: thread.project_id.clone(),
                title: "Competing edit".into(),
                objective: Some("Also edit".into()),
                repo_ids: vec![repo.id.clone()],
                ..Default::default()
            })
            .await
            .unwrap();
        let competing = core
            .create_collaboration_assignment(NewCollaborationAssignment {
                thread_id: second_thread.id,
                device_id: "device-edit".into(),
                agent: AgentKind::Codex,
                permission: "workspace_write".into(),
                execution_backend: ExecutionBackend::Host,
                message: None,
                client_message_id: None,
            })
            .await;
        assert!(
            competing.is_err(),
            "a repository may only have one shared writer"
        );

        let claimed = core
            .claim_collaboration_assignment(&assignment.id, "device-edit")
            .await
            .unwrap();
        am_vcs::create_worktree(&repo_path, &producer, "am-test-remote", &base).unwrap();
        std::fs::write(producer.join("file.txt"), "remote\n").unwrap();
        let diff = am_vcs::worktree_diff(&producer, &base, am_vcs::MAX_DIFF_BYTES).unwrap();
        let patch = am_vcs::worktree_patch_with_excludes(&producer, &base, &[]).unwrap();
        let change = core
            .report_collaboration_change_set(NewCollaborationChangeSet {
                assignment_id: assignment.id.clone(),
                lease_token: claimed.lease_token.clone(),
                repo_id: repo.id,
                base_ref: Some(base),
                files: diff.files,
                patch,
            })
            .await
            .unwrap();
        let finished = core
            .finish_collaboration_assignment(FinishCollaborationAssignment {
                assignment_id: assignment.id,
                lease_token: claimed.lease_token,
                state: SessionState::Completed,
                error: None,
            })
            .await
            .unwrap();
        assert_eq!(finished.status, CollaborationAssignmentStatus::Review);

        std::fs::write(repo_path.join("file.txt"), "local\n").unwrap();
        let conflict = core
            .apply_collaboration_change_set(&change.id, false)
            .await
            .unwrap();
        assert_eq!(conflict.status, CollaborationChangeStatus::Conflict);
        let applied = core
            .apply_collaboration_change_set(&change.id, true)
            .await
            .unwrap();
        assert_eq!(
            applied.status,
            CollaborationChangeStatus::AppliedWithOverwrite
        );
        assert_eq!(
            std::fs::read_to_string(repo_path.join("file.txt")).unwrap(),
            "remote\n"
        );
        assert_eq!(
            std::fs::read_to_string(
                dir.join("data")
                    .join("collaboration-backups")
                    .join(&change.id)
                    .join("file.txt")
            )
            .unwrap(),
            "local\n"
        );
        let settled = core
            .list_collaboration_assignments(None, true)
            .await
            .unwrap();
        assert!(
            settled.is_empty(),
            "review resolution releases the writer lease"
        );

        let _ = am_vcs::remove_worktree(&repo_path, &producer);
        core.shutdown().await;
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
