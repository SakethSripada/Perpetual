use am_proto::{
    new_id, now, AgentThreadEvent, AppEvent, ClaimedCollaborationAssignment,
    CollaborationAssignment, CollaborationAssignmentStatus, CollaborationChangeSet,
    CollaborationChangeStatus, CollaborationDevice, CollaborationEventInput, CollaborationSnapshot,
    FinishCollaborationAssignment, NewCollaborationAssignment, NewCollaborationChangeSet,
    RegisterCollaborationDevice, SessionState, TaskStatus,
};
use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};

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
        let device = am_db::repos::collaboration::upsert_device(&self.db.pool, &input).await?;
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
    ) -> Result<CollaborationSnapshot, CoreError> {
        self.expire_collaboration_leases().await?;
        Ok(CollaborationSnapshot {
            devices: am_db::repos::collaboration::list_devices(&self.db.pool).await?,
            assignments: am_db::repos::collaboration::list_assignments(&self.db.pool, None, false)
                .await?
                .into_iter()
                .filter(|assignment| thread_id.is_none_or(|id| assignment.thread_id == id))
                .collect(),
            change_sets: am_db::repos::collaboration::list_change_sets(&self.db.pool, thread_id)
                .await?,
            server_time: now(),
        })
    }

    pub async fn create_collaboration_assignment(
        &self,
        input: NewCollaborationAssignment,
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
        if !user_text.is_empty() {
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
        am_db::repos::collaboration::insert_assignment(&self.db.pool, &assignment).await?;

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
        let assignment = am_db::repos::collaboration::finish_assignment(
            &self.db.pool,
            &input.assignment_id,
            &hash_secret(&input.lease_token),
            status,
            input.error.as_deref(),
        )
        .await?
        .ok_or_else(stale_lease_error)?;
        am_db::repos::agent_turn::finish(&self.db.pool, &assignment.turn_id, input.state).await?;
        if let Some(mut thread) =
            am_db::repos::agent_thread::get(&self.db.pool, &assignment.thread_id).await?
        {
            thread.status = match input.state {
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
        AgentKind, CollaborationAgentCapability, ExecutionBackend, NewAgentThread, NewProject,
    };

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
        let finished = core
            .finish_collaboration_assignment(FinishCollaborationAssignment {
                assignment_id: queued.id.clone(),
                lease_token: claimed.lease_token.clone(),
                state: SessionState::Completed,
                error: None,
            })
            .await
            .unwrap();
        assert_eq!(finished.status, CollaborationAssignmentStatus::Review);

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
                assignment_id: queued.id,
                lease_token: claimed.lease_token,
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

        core.shutdown().await;
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
