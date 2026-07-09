//! Live permission approvals.
//!
//! When a run uses [`PermissionPolicy::Ask`], the agent must ask the user before
//! each gated action. Requests are held in an in-memory registry (the same
//! pattern as the queued-message map); each one publishes an
//! [`AppEvent::ApprovalRequested`] and parks on a oneshot until the UI resolves
//! it, the run ends, or the wait times out. Decisions route back by request id,
//! so attribution is exact regardless of how many runs are in flight.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use am_agents::{ApprovalAsk, ApprovalDecision, ApprovalResponder, PermissionPolicy};
use am_proto::{new_id, now, AgentKind, AppEvent, ApprovalRequest, ApprovalResolution};
use serde_json::json;
use tokio::sync::{oneshot, Mutex};

use crate::{AppCore, CoreError};

/// Run context attached to every approval so the UI can route the prompt to the
/// right task/thread/node. Core-internal; the wire-facing shape is
/// [`am_proto::ApprovalRequest`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ApprovalScope {
    pub project_id: Option<String>,
    pub work_node_id: Option<String>,
    pub task_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
}

/// How long to wait for a user decision before auto-denying. A backstop so a
/// forgotten prompt can never wedge an agent forever.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(900);

pub(crate) struct Pending {
    request: ApprovalRequest,
    tx: oneshot::Sender<ApprovalDecision>,
}

/// In-memory map of pending approvals, keyed by request id.
pub(crate) type ApprovalRegistry = Arc<Mutex<HashMap<String, Pending>>>;

pub(crate) fn new_registry() -> ApprovalRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

impl AppCore {
    /// Build the per-run approval callback for an adapter-driven agent (Codex).
    /// Claude routes approvals through its MCP `--permission-prompt-tool` instead,
    /// so it gets `None` here. Both `Ask` (prompt for everything) and
    /// `WorkspaceWrite`/Edit (auto-approve edits, prompt on escalation) want live
    /// approval; `ReadOnly` and `Autonomous` never prompt.
    pub(crate) fn approver_for(
        &self,
        permission: PermissionPolicy,
        agent: AgentKind,
        scope: ApprovalScope,
    ) -> Option<ApprovalResponder> {
        if agent != AgentKind::Codex
            || !matches!(
                permission,
                PermissionPolicy::Ask | PermissionPolicy::WorkspaceWrite
            )
        {
            return None;
        }
        let core = self.clone();
        Some(ApprovalResponder::new(move |ask| {
            let core = core.clone();
            let scope = scope.clone();
            Box::pin(async move { core.request_approval(scope, agent, ask).await })
        }))
    }

    /// Register a pending approval, broadcast it, and await the user's decision.
    /// Returns [`ApprovalDecision::Deny`] if the run ends or the wait times out.
    pub(crate) async fn request_approval(
        &self,
        scope: ApprovalScope,
        agent: AgentKind,
        ask: ApprovalAsk,
    ) -> ApprovalDecision {
        let request = ApprovalRequest {
            id: new_id(),
            agent,
            project_id: scope.project_id,
            work_node_id: scope.work_node_id,
            task_id: scope.task_id,
            thread_id: scope.thread_id,
            session_id: scope.session_id,
            kind: ask.kind,
            tool_name: ask.tool_name,
            command: ask.command,
            cwd: ask.cwd,
            input: ask.input,
            reason: ask.reason,
            created_at: now(),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.approvals.lock().await;
            pending.insert(
                request.id.clone(),
                Pending {
                    request: request.clone(),
                    tx,
                },
            );
        }
        let _ = self
            .activity(
                request.project_id.clone(),
                request.task_id.clone(),
                "approval.requested",
                json!({
                    "approval_id": request.id,
                    "agent": agent.as_str(),
                    "kind": request.kind.as_str(),
                    "tool_name": request.tool_name,
                }),
            )
            .await;
        self.events
            .publish(AppEvent::ApprovalRequested(request.clone()));

        match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            // `resolve_approval` already published the resolution + activity.
            Ok(Ok(decision)) => decision,
            // Sender dropped without a decision (run ended); the canceller
            // published the resolution.
            Ok(Err(_)) => ApprovalDecision::Deny,
            // Timed out: tear down the entry ourselves and announce it.
            Err(_) => {
                let removed = self.approvals.lock().await.remove(&request.id).is_some();
                if removed {
                    self.publish_resolution(
                        &request.id,
                        ApprovalResolution::TimedOut,
                        None,
                        request.project_id.as_deref(),
                        request.task_id.as_deref(),
                    )
                    .await;
                }
                ApprovalDecision::Deny
            }
        }
    }

    /// Apply a user decision to a pending approval. Errors if it is unknown
    /// (already resolved, timed out, or its run ended).
    pub async fn resolve_approval(
        &self,
        id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), CoreError> {
        let pending = self
            .approvals
            .lock()
            .await
            .remove(id)
            .ok_or(CoreError::NotFound)?;
        let request = pending.request;
        // Receiver may be gone if the run ended in the same instant; that's fine.
        let _ = pending.tx.send(decision);
        self.publish_resolution(
            id,
            ApprovalResolution::Decided,
            Some(decision),
            request.project_id.as_deref(),
            request.task_id.as_deref(),
        )
        .await;
        Ok(())
    }

    /// Request approval from the Claude MCP permission-prompt path, which only
    /// carries a run id (the task session id or thread turn id). Resolves the
    /// full scope from the DB so the prompt routes to the right place.
    pub async fn request_approval_for_run(
        &self,
        run_id: Option<&str>,
        agent: AgentKind,
        ask: ApprovalAsk,
    ) -> ApprovalDecision {
        let scope = match run_id {
            Some(id) => self.approval_scope_for_run(id).await,
            None => ApprovalScope::default(),
        };
        self.request_approval(scope, agent, ask).await
    }

    async fn approval_scope_for_run(&self, run_id: &str) -> ApprovalScope {
        if let Ok(Some(session)) = am_db::repos::session::get(&self.db.pool, run_id).await {
            let project_id = am_db::repos::task::get(&self.db.pool, &session.task_id)
                .await
                .ok()
                .flatten()
                .map(|task| task.project_id);
            let work_node_id =
                am_db::repos::work_graph::get_node_for_task(&self.db.pool, &session.task_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|node| node.id);
            return ApprovalScope {
                project_id,
                work_node_id,
                task_id: Some(session.task_id),
                thread_id: None,
                session_id: Some(run_id.to_string()),
            };
        }
        if let Ok(Some(turn)) = am_db::repos::agent_turn::get(&self.db.pool, run_id).await {
            let project_id = am_db::repos::agent_thread::get(&self.db.pool, &turn.thread_id)
                .await
                .ok()
                .flatten()
                .and_then(|thread| thread.project_id);
            let work_node_id =
                am_db::repos::work_graph::get_node_for_thread(&self.db.pool, &turn.thread_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|node| node.id);
            return ApprovalScope {
                project_id,
                work_node_id,
                task_id: None,
                thread_id: Some(turn.thread_id),
                session_id: Some(run_id.to_string()),
            };
        }
        ApprovalScope {
            session_id: Some(run_id.to_string()),
            ..Default::default()
        }
    }

    /// All in-flight approvals, for a freshly connected UI to render.
    pub async fn list_pending_approvals(&self) -> Vec<ApprovalRequest> {
        let mut items: Vec<ApprovalRequest> = self
            .approvals
            .lock()
            .await
            .values()
            .map(|p| p.request.clone())
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        items
    }

    /// Auto-deny and clear every approval belonging to a session/turn that has
    /// ended, so the agent (and any awaiting callback) never hangs. Dropping the
    /// sender resolves `request_approval` to a deny.
    pub(crate) async fn cancel_session_approvals(&self, session_id: &str) {
        let cancelled: Vec<ApprovalRequest> = {
            let mut pending = self.approvals.lock().await;
            let ids: Vec<String> = pending
                .iter()
                .filter(|(_, p)| p.request.session_id.as_deref() == Some(session_id))
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|p| p.request))
                .collect()
        };
        for request in cancelled {
            self.publish_resolution(
                &request.id,
                ApprovalResolution::Cancelled,
                None,
                request.project_id.as_deref(),
                request.task_id.as_deref(),
            )
            .await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn pending_count(&self) -> usize {
        self.approvals.lock().await.len()
    }

    async fn publish_resolution(
        &self,
        id: &str,
        resolution: ApprovalResolution,
        decision: Option<ApprovalDecision>,
        project_id: Option<&str>,
        task_id: Option<&str>,
    ) {
        let _ = self
            .activity(
                project_id.map(ToString::to_string),
                task_id.map(ToString::to_string),
                "approval.resolved",
                json!({
                    "approval_id": id,
                    "resolution": resolution.as_str(),
                    "decision": decision.map(|d| d.as_str()),
                }),
            )
            .await;
        self.events.publish(AppEvent::ApprovalResolved {
            id: id.to_string(),
            resolution,
            decision,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_agents::ApprovalKind;
    use std::time::Duration;

    async fn core() -> AppCore {
        let dir = std::env::temp_dir().join(format!("am-approval-test-{}", am_proto::new_id()));
        AppCore::new(&dir).await.unwrap()
    }

    fn ask() -> ApprovalAsk {
        ApprovalAsk {
            kind: ApprovalKind::Command,
            tool_name: "command".into(),
            command: Some(vec!["ls".into()]),
            cwd: None,
            input: serde_json::Value::Null,
            reason: None,
        }
    }

    fn scope() -> ApprovalScope {
        ApprovalScope {
            session_id: Some("sess-1".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn resolve_routes_decision_to_waiter() {
        let core = core().await;
        let task = {
            let core = core.clone();
            tokio::spawn(async move {
                core.request_approval(scope(), AgentKind::Codex, ask())
                    .await
            })
        };
        // Wait for the request to register, then resolve it.
        let id = loop {
            let pending = core.list_pending_approvals().await;
            if let Some(req) = pending.first() {
                break req.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        core.resolve_approval(&id, ApprovalDecision::Allow)
            .await
            .unwrap();
        assert_eq!(task.await.unwrap(), ApprovalDecision::Allow);
        assert_eq!(core.pending_count().await, 0);
    }

    #[tokio::test]
    async fn cancelling_session_denies_pending() {
        let core = core().await;
        let task = {
            let core = core.clone();
            tokio::spawn(async move {
                core.request_approval(scope(), AgentKind::Codex, ask())
                    .await
            })
        };
        loop {
            if core.pending_count().await > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        core.cancel_session_approvals("sess-1").await;
        assert_eq!(task.await.unwrap(), ApprovalDecision::Deny);
        assert_eq!(core.pending_count().await, 0);
    }

    #[tokio::test]
    async fn resolve_unknown_is_error() {
        let core = core().await;
        assert!(core
            .resolve_approval("nope", ApprovalDecision::Allow)
            .await
            .is_err());
    }
}
