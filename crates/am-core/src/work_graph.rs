use std::collections::{HashMap, HashSet, VecDeque};

use am_agents::PermissionPolicy;
use am_proto::{
    new_id, now, AgentKind, AppEvent, ContextInclusion, ContextPacket, EvaluationVerdict,
    ExecutionBackend, GateMode, LayoutMode, ModelTargetKind, NewAgentThread, NewTask, NewWorkEdge,
    NewWorkNode, QueuedWorkMessage, TaskStatus, TaskUpdate, WorkEdge, WorkEdgeKind,
    WorkEdgeUpdate, WorkGraph, WorkNode, WorkNodeDiff, WorkNodeKind, WorkNodeRepoBinding,
    WorkNodeUpdate, WorkPlanRun, WorkPlanRunState, WorkRun,
};
use serde_json::json;

use crate::agent_thread::{parse_permission, permission_to_string};
use crate::context_scoring::{rank_context_files, ScoreBoosts};
use crate::{AppCore, CoreError};

const CONTEXT_BUDGET_BYTES: i64 = 24 * 1024;
const CONTEXT_HARD_CAP_BYTES: i64 = 32 * 1024;
/// Token-aware budgeting (bytes/4 heuristic). The soft cap admits only
/// high-score items once crossed; the hard cap is absolute.
const CONTEXT_BUDGET_TOKENS: i64 = 6_000;
const CONTEXT_HARD_CAP_TOKENS: i64 = 8_000;
const LAYOUT_X_GAP: f64 = 300.0;
const LAYOUT_Y_GAP: f64 = 142.0;
const LAYOUT_X0: f64 = 80.0;
const LAYOUT_Y0: f64 = 80.0;

#[derive(Debug, Clone, Default)]
pub struct WorkRunModelOptions {
    pub model: Option<String>,
    pub model_target: Option<ModelTargetKind>,
    pub compute_profile: Option<String>,
    pub max_compute_usd: Option<f64>,
    pub allow_auto_purchase: bool,
}

/// Per-source token ceilings so one noisy source can't crowd out the rest.
fn source_token_budget(source_kind: &str) -> i64 {
    match source_kind {
        "repo_file" => 3_000,
        "memory" => 900,
        "doc" => 900,
        "handoff" | "sibling" => 1_200,
        kind if kind.starts_with("search:") => 600,
        // Core work-item context (node, parent, blockers, repos) is bounded
        // by count, not by a category cap.
        _ => i64::MAX,
    }
}

impl AppCore {
    pub async fn get_work_graph(&self, project_id: &str) -> Result<WorkGraph, CoreError> {
        Ok(am_db::repos::work_graph::graph(&self.db.pool, project_id).await?)
    }

    pub async fn get_work_node(&self, node_id: &str) -> Result<Option<WorkNode>, CoreError> {
        Ok(am_db::repos::work_graph::get_node(&self.db.pool, node_id).await?)
    }

    pub async fn get_work_run(&self, run_id: &str) -> Result<Option<WorkRun>, CoreError> {
        Ok(am_db::repos::work_graph::get_run(&self.db.pool, run_id).await?)
    }

    pub async fn list_work_runs(&self, node_id: &str) -> Result<Vec<WorkRun>, CoreError> {
        Ok(am_db::repos::work_graph::list_runs_for_node(&self.db.pool, node_id).await?)
    }

    /// List pending follow-up messages waiting to run on a work node, whether it
    /// is backed by an agent thread (persisted queue) or a task (in-memory
    /// steering queue).
    pub async fn list_queued_work_messages(
        &self,
        node_id: &str,
    ) -> Result<Vec<QueuedWorkMessage>, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if let Some(thread_id) = &node.thread_id {
            let queued = self.list_queued_turns(thread_id).await?;
            Ok(queued
                .into_iter()
                .map(|turn| QueuedWorkMessage {
                    node_id: node.id.clone(),
                    agent_kind: turn.agent_kind,
                    message: turn.message,
                })
                .collect())
        } else if let Some(task_id) = &node.task_id {
            let queues = self.messages.lock().await;
            Ok(queues
                .get(task_id)
                .map(|queue| {
                    queue
                        .iter()
                        .map(|msg| QueuedWorkMessage {
                            node_id: node.id.clone(),
                            agent_kind: msg.agent,
                            message: msg.message.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default())
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn create_work_node(&self, mut input: NewWorkNode) -> Result<WorkNode, CoreError> {
        let kind = input.kind.unwrap_or(WorkNodeKind::Task);
        // No explicit coordinates means the caller (typically an MCP agent)
        // doesn't care about placement: give a provisional slot now and let
        // the debounced auto-layout arrange the burst once it settles.
        let auto_layout = input.position_x.is_none() || input.position_y.is_none();
        if auto_layout {
            let (x, y) = self
                .next_work_node_position(&input.project_id, input.parent_id.as_deref(), kind)
                .await?;
            input.position_x.get_or_insert(x);
            input.position_y.get_or_insert(y);
        }
        let repo_ids = input.repo_ids.clone();
        let mut node = match kind {
            WorkNodeKind::Task => {
                let task = self
                    .create_task(NewTask {
                        project_id: input.project_id.clone(),
                        title: input.title.clone(),
                        repo_id: repo_ids.first().cloned(),
                        description: input.description.clone(),
                        priority: input.priority,
                        primary_agent: input.primary_agent,
                        model: input.model.clone(),
                        model_target: input.model_target,
                        compute_lease_id: None,
                        compute_provider: input.compute_provider,
                        estimated_compute_cost_usd: input.max_compute_usd,
                        fallback_model_target: None,
                    })
                    .await?;
                let mut node = am_db::repos::work_graph::get_node_for_task(&self.db.pool, &task.id)
                    .await?
                    .ok_or(CoreError::NotFound)?;
                node = self
                    .update_work_node(
                        &node.id,
                        WorkNodeUpdate {
                            parent_id: input.parent_id.clone(),
                            position_x: input.position_x,
                            position_y: input.position_y,
                            ..Default::default()
                        },
                    )
                    .await?;
                if repo_ids.len() > 1 {
                    self.assign_work_node_repos(&node.id, repo_ids.clone())
                        .await?;
                }
                node
            }
            WorkNodeKind::Session => {
                let thread = self
                    .create_agent_thread(NewAgentThread {
                        project_id: Some(input.project_id.clone()),
                        group_id: None,
                        title: input.title.clone(),
                        objective: input.description.clone(),
                        repo_ids: repo_ids.clone(),
                        preferred_agent: input.primary_agent,
                        permission: None,
                        execution_backend: None,
                        model: input.model.clone(),
                        reasoning: None,
                        local_provider: None,
                        local_base_url: input.compute_profile.clone(),
                        model_target: input.model_target,
                        compute_lease_id: None,
                        compute_provider: input.compute_provider,
                        estimated_compute_cost_usd: input.max_compute_usd,
                        fallback_model_target: None,
                        sort_order: None,
                    })
                    .await?;
                let mut node =
                    am_db::repos::work_graph::get_node_for_thread(&self.db.pool, &thread.id)
                        .await?
                        .ok_or(CoreError::NotFound)?;
                node = self
                    .update_work_node(
                        &node.id,
                        WorkNodeUpdate {
                            parent_id: input.parent_id.clone(),
                            position_x: input.position_x,
                            position_y: input.position_y,
                            ..Default::default()
                        },
                    )
                    .await?;
                node
            }
            WorkNodeKind::Group | WorkNodeKind::Milestone => {
                am_db::repos::work_graph::create_standalone_node(&self.db.pool, input, kind).await?
            }
        };

        if matches!(kind, WorkNodeKind::Group | WorkNodeKind::Milestone) && !repo_ids.is_empty() {
            self.assign_work_node_repos(&node.id, repo_ids).await?;
            node = am_db::repos::work_graph::get_node(&self.db.pool, &node.id)
                .await?
                .ok_or(CoreError::NotFound)?;
        }

        self.events.publish(AppEvent::WorkNodeCreated(node.clone()));
        self.activity(
            Some(node.project_id.clone()),
            node.task_id.clone(),
            "work.node_created",
            json!({ "node_id": node.id, "kind": node.kind.as_str(), "title": node.title }),
        )
        .await?;
        if auto_layout {
            self.schedule_auto_prettify(&node.project_id);
        }
        Ok(node)
    }

    async fn next_work_node_position(
        &self,
        project_id: &str,
        parent_id: Option<&str>,
        kind: WorkNodeKind,
    ) -> Result<(f64, f64), CoreError> {
        let graph = am_db::repos::work_graph::graph(&self.db.pool, project_id).await?;
        if graph.nodes.is_empty() {
            return Ok((LAYOUT_X0, LAYOUT_Y0));
        }
        let parent_key = parent_id.map(ToString::to_string);
        let siblings: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.parent_id == parent_key)
            .collect();
        let index = siblings.len() as f64;
        if parent_id.is_some() && kind != WorkNodeKind::Group {
            return Ok((LAYOUT_X0 + 24.0, LAYOUT_Y0 + index * 110.0));
        }
        let max_x = graph
            .nodes
            .iter()
            .map(|node| node.position_x)
            .fold(LAYOUT_X0, f64::max);
        let row = (index % 6.0).floor();
        Ok((max_x + LAYOUT_X_GAP, LAYOUT_Y0 + row * LAYOUT_Y_GAP))
    }

    pub async fn update_work_node(
        &self,
        node_id: &str,
        patch: WorkNodeUpdate,
    ) -> Result<WorkNode, CoreError> {
        let original_patch = patch.clone();
        let before = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;

        if let Some(task_id) = before.task_id.clone() {
            let task_patch = TaskUpdate {
                title: patch.title.clone(),
                description: patch.description.clone(),
                status: patch.status,
                priority: patch.priority,
                primary_agent: patch.primary_agent,
                ..Default::default()
            };
            let has_task_change = task_patch.title.is_some()
                || task_patch.description.is_some()
                || task_patch.status.is_some()
                || task_patch.priority.is_some()
                || task_patch.primary_agent.is_some();
            if has_task_change {
                self.update_task(&task_id, task_patch).await?;
            }
        } else if let Some(thread_id) = before.thread_id.clone() {
            let thread_patch = am_proto::AgentThreadUpdate {
                title: patch.title.clone(),
                status: patch.status,
                active_agent: patch.primary_agent,
                preferred_agent: patch.primary_agent,
                objective: patch.description.clone(),
                ..Default::default()
            };
            let has_thread_change = thread_patch.title.is_some()
                || thread_patch.status.is_some()
                || thread_patch.active_agent.is_some()
                || thread_patch.objective.is_some();
            if has_thread_change {
                self.update_agent_thread(&thread_id, thread_patch).await?;
            }
        }

        let graph_patch = if before.task_id.is_none() && before.thread_id.is_none() {
            original_patch
        } else {
            WorkNodeUpdate {
                parent_id: patch.parent_id,
                position_x: patch.position_x,
                position_y: patch.position_y,
                sort_order: patch.sort_order,
                ..Default::default()
            }
        };
        let node =
            am_db::repos::work_graph::update_node_fields(&self.db.pool, node_id, graph_patch)
                .await?;
        self.events.publish(AppEvent::WorkNodeUpdated(node.clone()));
        self.notify_plan_watchers(&node.project_id);
        self.activity(
            Some(node.project_id.clone()),
            node.task_id.clone(),
            "work.node_updated",
            json!({ "node_id": node.id, "status": node.status.as_str() }),
        )
        .await?;
        if before.status != TaskStatus::Done && node.status == TaskStatus::Done {
            self.resume_plans_unblocked_by_node(&node).await?;
        }
        Ok(node)
    }

    async fn resume_plans_unblocked_by_node(&self, node: &WorkNode) -> Result<(), CoreError> {
        let plans =
            am_db::repos::work_graph::list_plan_runs(&self.db.pool, &node.project_id).await?;
        let steer_enabled = plans.iter().any(|plan| {
            plan.state == WorkPlanRunState::Running && plan.steer_dependents_on_unblock
        });
        for plan in plans
            .into_iter()
            .filter(|plan| plan.state == WorkPlanRunState::Paused)
            .filter(|plan| plan.resume_after_node_id.as_deref() == Some(node.id.as_str()))
        {
            let core = self.clone();
            tokio::spawn(async move {
                let _ = core.resume_work_plan_boxed(plan.id).await;
            });
        }

        // Announce the resolution to each dependent (drives UI badges and the
        // activity trail), and optionally steer dependents that are already
        // mid-session with the completed work's handoff summary.
        let dependents =
            am_db::repos::work_graph::gating_dependents(&self.db.pool, &node.id).await?;
        if dependents.is_empty() {
            return Ok(());
        }
        let handoff = match node.task_id.as_deref() {
            Some(task_id) => {
                am_db::repos::task_context::latest_handoff(&self.db.pool, task_id).await?
            }
            None => None,
        };
        for dependent in dependents {
            self.activity(
                Some(dependent.project_id.clone()),
                dependent.task_id.clone(),
                "work.blocker_resolved",
                json!({
                    "node_id": dependent.id,
                    "blocker_id": node.id,
                    "blocker_title": node.title,
                }),
            )
            .await?;

            if !steer_enabled {
                continue;
            }
            let session_key = dependent
                .task_id
                .as_deref()
                .or(dependent.thread_id.as_deref())
                .unwrap_or(dependent.id.as_str());
            if !self.sessions.is_active(session_key).await {
                continue;
            }
            let mut message = format!(
                "Coordination update: prerequisite \"{}\" just completed.",
                node.title
            );
            if let Some(handoff) = &handoff {
                message.push_str("\n\nIts handoff summary:\n");
                message.push_str(&truncate(&handoff.summary, 1_200));
            }
            let agent = dependent.primary_agent.unwrap_or(AgentKind::ClaudeCode);
            let _ = self
                .send_work_node_message(
                    &dependent.id,
                    agent,
                    PermissionPolicy::WorkspaceWrite,
                    message,
                )
                .await;
        }
        Ok(())
    }

    pub async fn delete_work_node(&self, node_id: &str) -> Result<(), CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        // Task-backed work is owned by its task: deleting the task also removes
        // this node and logs its own activity, so we're done.
        if let Some(task_id) = &node.task_id {
            self.delete_task(task_id).await?;
            return Ok(());
        }
        if let Some(thread_id) = &node.thread_id {
            self.delete_agent_thread(thread_id, false).await?;
        }
        am_db::repos::work_graph::delete_node(&self.db.pool, node_id).await?;
        self.activity(
            Some(node.project_id),
            None,
            "work.node_deleted",
            json!({ "node_id": node_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn move_work_node(
        &self,
        node_id: &str,
        parent_id: Option<String>,
        position_x: f64,
        position_y: f64,
    ) -> Result<WorkNode, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if parent_id.as_deref() == Some(node_id) {
            return Err(CoreError::Other(
                "a work node cannot be its own parent".into(),
            ));
        }
        if let Some(parent_id) = parent_id.as_deref() {
            let parent = am_db::repos::work_graph::get_node(&self.db.pool, parent_id)
                .await?
                .ok_or(CoreError::NotFound)?;
            if parent.project_id != node.project_id {
                return Err(CoreError::Other(
                    "parent work node must be in the same project".into(),
                ));
            }
        }
        let moved = am_db::repos::work_graph::move_node(
            &self.db.pool,
            node_id,
            parent_id,
            position_x,
            position_y,
        )
        .await?;
        self.events
            .publish(AppEvent::WorkNodeUpdated(moved.clone()));
        self.activity(
            Some(moved.project_id.clone()),
            moved.task_id.clone(),
            "work.node_moved",
            json!({ "node_id": moved.id, "parent_id": moved.parent_id }),
        )
        .await?;
        Ok(moved)
    }

    pub async fn connect_work_nodes(&self, input: NewWorkEdge) -> Result<WorkEdge, CoreError> {
        let source = am_db::repos::work_graph::get_node(&self.db.pool, &input.source_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let target = am_db::repos::work_graph::get_node(&self.db.pool, &input.target_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if source.project_id != input.project_id || target.project_id != input.project_id {
            return Err(CoreError::Other(
                "both work nodes must belong to the edge project".into(),
            ));
        }
        if input.source_id == input.target_id {
            return Err(CoreError::Other("a work link cannot target itself".into()));
        }
        self.validate_gating_edge_candidate(
            &input.project_id,
            None,
            &input.source_id,
            &input.target_id,
            input.kind,
        )
        .await?;
        let edge = am_db::repos::work_graph::create_edge(&self.db.pool, input).await?;
        self.events.publish(AppEvent::WorkGraphUpdated {
            project_id: edge.project_id.clone(),
        });
        self.activity(
            Some(edge.project_id.clone()),
            None,
            "work.edge_connected",
            json!({
                "edge_id": edge.id,
                "source_id": edge.source_id,
                "target_id": edge.target_id,
                "kind": edge.kind.as_str(),
            }),
        )
        .await?;
        // Dependencies define the layered structure: re-arrange unpinned
        // nodes once the current burst of edits settles. Only when neither
        // endpoint was manually placed — respect deliberate arrangements.
        if !source.position_locked && !target.position_locked {
            self.schedule_auto_prettify(&edge.project_id);
        }
        Ok(edge)
    }

    pub async fn update_work_edge(
        &self,
        edge_id: &str,
        patch: WorkEdgeUpdate,
    ) -> Result<WorkEdge, CoreError> {
        let before = am_db::repos::work_graph::get_edge(&self.db.pool, edge_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let source_id = patch
            .source_id
            .clone()
            .unwrap_or_else(|| before.source_id.clone());
        let target_id = patch
            .target_id
            .clone()
            .unwrap_or_else(|| before.target_id.clone());
        let kind = patch.kind.unwrap_or(before.kind);
        if source_id == target_id {
            return Err(CoreError::Other("a work link cannot target itself".into()));
        }
        let source = am_db::repos::work_graph::get_node(&self.db.pool, &source_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let target = am_db::repos::work_graph::get_node(&self.db.pool, &target_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if source.project_id != before.project_id || target.project_id != before.project_id {
            return Err(CoreError::Other(
                "both work nodes must stay in the edge project".into(),
            ));
        }
        self.validate_gating_edge_candidate(
            &before.project_id,
            Some(edge_id),
            &source_id,
            &target_id,
            kind,
        )
        .await?;
        let edge = am_db::repos::work_graph::update_edge(&self.db.pool, edge_id, patch).await?;
        self.events.publish(AppEvent::WorkGraphUpdated {
            project_id: edge.project_id.clone(),
        });
        self.activity(
            Some(edge.project_id.clone()),
            None,
            "work.edge_updated",
            json!({
                "edge_id": edge.id,
                "source_id": edge.source_id,
                "target_id": edge.target_id,
                "kind": edge.kind.as_str(),
            }),
        )
        .await?;
        Ok(edge)
    }

    pub async fn disconnect_work_nodes(&self, edge_id: &str) -> Result<(), CoreError> {
        let edge = am_db::repos::work_graph::get_edge(&self.db.pool, edge_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        am_db::repos::work_graph::delete_edge(&self.db.pool, edge_id).await?;
        self.events.publish(AppEvent::WorkGraphUpdated {
            project_id: edge.project_id.clone(),
        });
        self.activity(
            Some(edge.project_id.clone()),
            None,
            "work.edge_disconnected",
            json!({
                "edge_id": edge.id,
                "source_id": edge.source_id,
                "target_id": edge.target_id,
                "kind": edge.kind.as_str(),
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn prettify_work_graph(
        &self,
        project_id: &str,
        mode: LayoutMode,
    ) -> Result<WorkGraph, CoreError> {
        let graph = am_db::repos::work_graph::graph(&self.db.pool, project_id).await?;
        validate_gating_edges_acyclic(&graph.nodes, &graph.edges)?;
        let placements = crate::layout::compute_layout(&graph.nodes, &graph.edges, mode);
        // One transaction for the whole arrangement; Force also clears manual
        // pins so the next PreserveManual starts from a clean slate.
        am_db::repos::work_graph::apply_layout(
            &self.db.pool,
            project_id,
            &placements,
            mode == LayoutMode::Force,
        )
        .await?;
        let graph = am_db::repos::work_graph::graph(&self.db.pool, project_id).await?;
        self.events.publish(AppEvent::WorkGraphUpdated {
            project_id: project_id.to_string(),
        });
        self.activity(
            Some(project_id.to_string()),
            None,
            "work.graph_prettified",
            json!({ "mode": mode.as_str(), "node_count": graph.nodes.len() }),
        )
        .await?;
        Ok(graph)
    }

    /// Debounced automatic layout: bulk node/edge creation (an MCP agent
    /// decomposing a milestone, an import) triggers one PreserveManual pass
    /// after the burst settles instead of leaving a wall of default positions.
    pub(crate) fn schedule_auto_prettify(&self, project_id: &str) {
        let mut debounce = self.layout_debounce.lock().unwrap();
        if let Some(handle) = debounce.remove(project_id) {
            handle.abort();
        }
        let core = self.clone();
        let project = project_id.to_string();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let _ = core
                .prettify_work_graph(&project, LayoutMode::PreserveManual)
                .await;
        });
        debounce.insert(project_id.to_string(), handle);
    }

    pub async fn assign_work_node_repos(
        &self,
        node_id: &str,
        repo_ids: Vec<String>,
    ) -> Result<Vec<WorkNodeRepoBinding>, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        for repo_id in &repo_ids {
            self.validate_project_repo(&node.project_id, repo_id)
                .await?;
        }

        if let Some(thread_id) = &node.thread_id {
            self.assign_thread_repos(thread_id, repo_ids.clone())
                .await?;
        } else if let Some(task_id) = &node.task_id {
            if let Some(repo_id) = repo_ids.first() {
                self.assign_task_repo(task_id, repo_id).await?;
            } else {
                am_db::repos::task_repo::clear_for_task(&self.db.pool, task_id).await?;
            }
        }

        let bindings =
            am_db::repos::work_graph::replace_node_repos(&self.db.pool, node_id, &repo_ids).await?;
        self.activity(
            Some(node.project_id),
            node.task_id,
            "work.repos_selected",
            json!({ "node_id": node_id, "repo_count": repo_ids.len() }),
        )
        .await?;
        Ok(bindings)
    }

    pub async fn run_work_node(
        &self,
        node_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, CoreError> {
        self.run_work_node_with_model_options(
            node_id,
            agent,
            permission,
            execution_backend,
            WorkRunModelOptions::default(),
        )
        .await
    }

    pub async fn run_work_node_with_model_options(
        &self,
        node_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
        model_options: WorkRunModelOptions,
    ) -> Result<String, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let blockers =
            am_db::repos::work_graph::blocking_edges_for_node(&self.db.pool, node_id).await?;
        if !blockers.is_empty() {
            return Err(CoreError::Other(format!(
                "work is blocked by {} unfinished prerequisite(s)",
                blockers.len()
            )));
        }

        let locks_needed = permission != PermissionPolicy::ReadOnly;
        if locks_needed {
            let conflicts =
                am_db::repos::work_graph::conflicting_locks(&self.db.pool, node_id).await?;
            if !conflicts.is_empty() {
                return Err(CoreError::Other(format!(
                    "repository is already locked by {} running work item(s)",
                    conflicts.len()
                )));
            }
            am_db::repos::work_graph::acquire_locks(&self.db.pool, node_id).await?;
        }

        let packet = self.build_context_packet(&node).await?;
        am_db::repos::work_graph::record_context_packet(&self.db.pool, &packet).await?;
        let packet_prompt = render_context_packet_prompt(&packet);
        self.set_work_node_model_options(&node, &model_options)
            .await?;

        let result = if let Some(task_id) = &node.task_id {
            self.run_task_inner(
                task_id,
                agent,
                permission,
                Some(packet_prompt),
                execution_backend,
            )
            .await
        } else if let Some(thread_id) = &node.thread_id {
            self.run_agent_thread_with_backend(
                thread_id,
                agent,
                permission,
                Some(packet_prompt),
                execution_backend,
            )
            .await
        } else {
            Err(CoreError::Other(
                "groups and milestones cannot run agents directly".into(),
            ))
        };

        match result {
            Ok(run_ref) => {
                let _ = am_db::repos::work_graph::record_run(&self.db.pool, &node, agent, &run_ref)
                    .await;
                Ok(run_ref)
            }
            Err(err) => {
                if locks_needed {
                    let _ = am_db::repos::work_graph::release_locks(&self.db.pool, node_id).await;
                }
                Err(err)
            }
        }
    }

    pub async fn set_work_node_model_options(
        &self,
        node: &WorkNode,
        options: &WorkRunModelOptions,
    ) -> Result<(), CoreError> {
        if options.model.is_none()
            && options.model_target.is_none()
            && options.max_compute_usd.is_none()
            && options.compute_profile.is_none()
        {
            return Ok(());
        }
        if let Some(model_target) = options.model_target {
            if model_target == ModelTargetKind::RentedCompute {
                return Err(CoreError::Other(
                    "rented compute targets are not supported by the VS Code extension".into(),
                ));
            }
        }
        if let Some(task_id) = &node.task_id {
            let patch = TaskUpdate {
                model: options.model.clone(),
                model_target: options.model_target,
                estimated_compute_cost_usd: options.max_compute_usd,
                ..Default::default()
            };
            let updated = am_db::repos::task::update(&self.db.pool, task_id, patch).await?;
            self.events.publish(AppEvent::TaskUpdated(updated));
        } else if let Some(thread_id) = &node.thread_id {
            let patch = am_proto::AgentThreadUpdate {
                model: options.model.clone(),
                model_target: options.model_target,
                local_base_url: options.compute_profile.clone(),
                estimated_compute_cost_usd: options.max_compute_usd,
                ..Default::default()
            };
            let updated =
                am_db::repos::agent_thread::update(&self.db.pool, thread_id, patch).await?;
            self.events.publish(AppEvent::AgentThreadUpdated(updated));
        }
        Ok(())
    }

    pub async fn run_work_plan(
        &self,
        project_id: &str,
        gate_mode: GateMode,
        max_active_runs: Option<i64>,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<WorkPlanRun, CoreError> {
        self.run_work_plan_with_options(
            project_id,
            gate_mode,
            max_active_runs,
            agent,
            permission,
            execution_backend,
            am_proto::WorkPlanOptions::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_work_plan_with_options(
        &self,
        project_id: &str,
        gate_mode: GateMode,
        max_active_runs: Option<i64>,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
        options: am_proto::WorkPlanOptions,
    ) -> Result<WorkPlanRun, CoreError> {
        let graph = am_db::repos::work_graph::graph(&self.db.pool, project_id).await?;
        validate_gating_edges_acyclic(&graph.nodes, &graph.edges)?;
        let total_count = graph
            .nodes
            .iter()
            .filter(|node| node.kind != WorkNodeKind::Group)
            .count() as i64;
        if total_count == 0 {
            return Err(CoreError::Other(
                "create runnable work or milestones before running a plan".into(),
            ));
        }
        let effective_capacity = self.effective_session_capacity().await as i64;
        let max_active_runs = max_active_runs
            .unwrap_or(effective_capacity)
            .clamp(1, effective_capacity);
        let evaluator_policy_json = self
            .get_evaluator_policy()
            .await
            .ok()
            .and_then(|policy| serde_json::to_string(&policy).ok());
        let plan = am_db::repos::work_graph::create_plan_run(
            &self.db.pool,
            project_id,
            gate_mode,
            max_active_runs,
            agent,
            &permission_to_string(permission),
            execution_backend,
            evaluator_policy_json.as_deref(),
            total_count,
            &options,
        )
        .await?;
        self.events
            .publish(AppEvent::WorkPlanRunUpdated(plan.clone()));
        self.activity(
            Some(project_id.to_string()),
            None,
            "work.plan_started",
            json!({
                "plan_run_id": plan.id,
                "gate_mode": gate_mode.as_str(),
                "max_active_runs": max_active_runs,
                "failure_mode": options.failure_mode.as_str(),
            }),
        )
        .await?;

        let core = self.clone();
        let plan_id = plan.id.clone();
        let project_id = project_id.to_string();
        let model_options = WorkRunModelOptions {
            model: options.model.clone(),
            model_target: options.model_target,
            compute_profile: options.compute_profile.clone(),
            max_compute_usd: options.max_compute_usd,
            allow_auto_purchase: options.allow_auto_purchase,
        };
        tokio::spawn(async move {
            core.drive_work_plan_boxed(
                plan_id,
                project_id,
                agent,
                permission,
                execution_backend,
                model_options,
            )
            .await;
        });
        Ok(plan)
    }

    pub async fn stop_work_plan(&self, plan_run_id: &str) -> Result<WorkPlanRun, CoreError> {
        let plan = am_db::repos::work_graph::cancel_plan_run(&self.db.pool, plan_run_id).await?;
        self.events
            .publish(AppEvent::WorkPlanRunUpdated(plan.clone()));
        for run in
            am_db::repos::work_graph::list_running_runs_for_plan(&self.db.pool, plan_run_id).await?
        {
            let _ = self.stop_work_node(&run.node_id).await;
        }
        self.activity(
            Some(plan.project_id.clone()),
            None,
            "work.plan_cancelled",
            json!({ "plan_run_id": plan.id }),
        )
        .await?;
        Ok(plan)
    }

    pub async fn resume_work_plan(&self, plan_run_id: &str) -> Result<WorkPlanRun, CoreError> {
        let plan = am_db::repos::work_graph::get_plan_run(&self.db.pool, plan_run_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if plan.state != WorkPlanRunState::Paused {
            return Ok(plan);
        }
        let graph = am_db::repos::work_graph::graph(&self.db.pool, &plan.project_id).await?;
        if plan.gate_mode == GateMode::Manual && manual_gate_pending(&graph) {
            return Err(CoreError::Other(
                "manual gate is still waiting for review".into(),
            ));
        }
        let resumed = am_db::repos::work_graph::resume_plan_run(&self.db.pool, plan_run_id).await?;
        self.events
            .publish(AppEvent::WorkPlanRunUpdated(resumed.clone()));
        let agent = resumed.default_agent.unwrap_or(AgentKind::Codex);
        let permission = resumed
            .default_permission
            .as_deref()
            .map(parse_permission)
            .unwrap_or(PermissionPolicy::WorkspaceWrite);
        let execution_backend = resumed.default_execution_backend;
        let core = self.clone();
        let plan_id = resumed.id.clone();
        let project_id = resumed.project_id.clone();
        tokio::spawn(async move {
            core.drive_work_plan_boxed(
                plan_id,
                project_id,
                agent,
                permission,
                execution_backend,
                WorkRunModelOptions::default(),
            )
            .await;
        });
        self.activity(
            Some(resumed.project_id.clone()),
            None,
            "work.plan_resumed",
            json!({ "plan_run_id": resumed.id }),
        )
        .await?;
        Ok(resumed)
    }

    fn resume_work_plan_boxed(
        &self,
        plan_run_id: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<WorkPlanRun, CoreError>> + Send + '_>,
    > {
        Box::pin(async move { self.resume_work_plan(&plan_run_id).await })
    }

    pub async fn get_work_plan_run(
        &self,
        plan_run_id: &str,
    ) -> Result<Option<WorkPlanRun>, CoreError> {
        Ok(am_db::repos::work_graph::get_plan_run(&self.db.pool, plan_run_id).await?)
    }

    pub async fn list_work_plan_runs(
        &self,
        project_id: &str,
    ) -> Result<Vec<WorkPlanRun>, CoreError> {
        Ok(am_db::repos::work_graph::list_plan_runs(&self.db.pool, project_id).await?)
    }

    pub async fn stop_work_node(&self, node_id: &str) -> Result<(), CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if let Some(task_id) = &node.task_id {
            self.stop_task(task_id).await?;
        } else if let Some(thread_id) = &node.thread_id {
            self.stop_agent_thread(thread_id).await?;
        } else {
            return Err(CoreError::Other("work node is not running".into()));
        }
        let _ = am_db::repos::work_graph::release_locks(&self.db.pool, node_id).await;
        Ok(())
    }

    pub async fn send_work_node_message(
        &self,
        node_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
    ) -> Result<Option<String>, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        if let Some(task_id) = &node.task_id {
            self.send_message(task_id, agent, permission, message).await
        } else if let Some(thread_id) = &node.thread_id {
            self.send_thread_message(thread_id, agent, permission, message)
                .await
        } else {
            Err(CoreError::Other(
                "groups and milestones cannot receive agent messages".into(),
            ))
        }
    }

    pub async fn preview_context_packet(&self, node_id: &str) -> Result<ContextPacket, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        self.build_context_packet(&node).await
    }

    pub async fn work_node_diff(&self, node_id: &str) -> Result<WorkNodeDiff, CoreError> {
        let node = am_db::repos::work_graph::get_node(&self.db.pool, node_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let task = match &node.task_id {
            Some(task_id) => Some(self.task_diff(task_id).await.unwrap_or_default()),
            None => None,
        };
        let thread = match &node.thread_id {
            Some(thread_id) => Some(self.thread_diff(thread_id).await.unwrap_or_default()),
            None => None,
        };
        Ok(WorkNodeDiff { task, thread })
    }

    /// The waker plan-run drivers for `project_id` sleep on. One waker per
    /// project; multiple concurrent drivers share it (notify_waiters wakes all).
    pub(crate) fn plan_waker(&self, project_id: &str) -> std::sync::Arc<tokio::sync::Notify> {
        self.plan_wakers
            .lock()
            .unwrap()
            .entry(project_id.to_string())
            .or_default()
            .clone()
    }

    /// Nudge any plan-run driver watching `project_id` to re-evaluate now
    /// (node/task state changed, a run ended, locks were released).
    pub(crate) fn notify_plan_watchers(&self, project_id: &str) {
        let waker = self.plan_wakers.lock().unwrap().get(project_id).cloned();
        if let Some(waker) = waker {
            waker.notify_waiters();
        }
    }

    pub(crate) async fn drive_work_plan(
        &self,
        plan_run_id: String,
        project_id: String,
        default_agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
        model_options: WorkRunModelOptions,
    ) {
        let waker = self.plan_waker(&project_id);
        loop {
            // Register interest before reading state: a notify fired while this
            // iteration works is buffered by this future, not lost.
            let notified = waker.notified();
            tokio::pin!(notified);
            let Some(plan) =
                (match am_db::repos::work_graph::get_plan_run(&self.db.pool, &plan_run_id).await {
                    Ok(plan) => plan,
                    Err(err) => {
                        let _ = self
                            .activity(
                                Some(project_id.clone()),
                                None,
                                "work.plan_error",
                                json!({ "plan_run_id": plan_run_id, "error": err.to_string() }),
                            )
                            .await;
                        return;
                    }
                })
            else {
                return;
            };
            if plan.state != WorkPlanRunState::Running {
                return;
            }

            let graph = match am_db::repos::work_graph::graph(&self.db.pool, &project_id).await {
                Ok(graph) => graph,
                Err(err) => {
                    self.finish_plan_run(
                        &plan_run_id,
                        WorkPlanRunState::Failed,
                        0,
                        0,
                        0,
                        Some(err.to_string()),
                    )
                    .await;
                    return;
                }
            };
            if let Err(err) = validate_gating_edges_acyclic(&graph.nodes, &graph.edges) {
                self.finish_plan_run(
                    &plan_run_id,
                    WorkPlanRunState::Failed,
                    0,
                    0,
                    0,
                    Some(err.to_string()),
                )
                .await;
                return;
            }

            let gate_outcome = self
                .apply_gate_mode(&plan, &graph)
                .await
                .unwrap_or(PlanGateOutcome::Continue);
            if gate_outcome == PlanGateOutcome::Paused {
                let (completed, active, blocked) = plan_counts(&graph);
                self.update_plan_run(
                    &plan_run_id,
                    WorkPlanRunState::Paused,
                    completed,
                    active,
                    blocked,
                    Some("manual gate is waiting for review".to_string()),
                    false,
                )
                .await;
                // Dedicated signal for the UI to prompt review of the gate.
                let gate_node = am_db::repos::work_graph::get_plan_run(&self.db.pool, &plan_run_id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|plan| plan.resume_after_node_id);
                let _ = self
                    .activity(
                        Some(project_id.clone()),
                        None,
                        "work.plan_gate_paused",
                        json!({ "plan_run_id": plan_run_id, "gate_node_id": gate_node }),
                    )
                    .await;
                return;
            }

            // Reload only when gate logic changed node statuses; otherwise the
            // graph loaded above is still current.
            let graph = if gate_outcome == PlanGateOutcome::ContinueUpdated {
                match am_db::repos::work_graph::graph(&self.db.pool, &project_id).await {
                    Ok(fresh) => fresh,
                    Err(_) => graph,
                }
            } else {
                graph
            };
            let (completed, active, blocked) = plan_counts(&graph);
            let failed_ids: Vec<String> = graph
                .nodes
                .iter()
                .filter(|node| {
                    node.kind != WorkNodeKind::Group && node.status == TaskStatus::Failed
                })
                .map(|node| node.id.clone())
                .collect();
            if !failed_ids.is_empty() {
                match plan.failure_mode {
                    am_proto::PlanFailureMode::Halt => {
                        self.finish_plan_run(
                            &plan_run_id,
                            WorkPlanRunState::Failed,
                            completed,
                            active,
                            blocked,
                            Some("one or more work nodes failed".to_string()),
                        )
                        .await;
                        return;
                    }
                    am_proto::PlanFailureMode::Retry => {
                        let retried = self
                            .retry_failed_plan_nodes(&plan, &failed_ids)
                            .await
                            .unwrap_or(false);
                        if !retried {
                            self.finish_plan_run(
                                &plan_run_id,
                                WorkPlanRunState::Failed,
                                completed,
                                active,
                                blocked,
                                Some(format!(
                                    "{} node(s) failed after exhausting {} retr{}",
                                    failed_ids.len(),
                                    plan.max_node_retries,
                                    if plan.max_node_retries == 1 {
                                        "y"
                                    } else {
                                        "ies"
                                    }
                                )),
                            )
                            .await;
                            return;
                        }
                        // Requeued nodes become ready below on the next pass.
                    }
                    am_proto::PlanFailureMode::Continue => {
                        // Failed subtrees are skipped; independent work keeps
                        // running. Completion handling below excludes them.
                    }
                }
            }
            let total = graph
                .nodes
                .iter()
                .filter(|node| node.kind != WorkNodeKind::Group)
                .count() as i64;
            // In continue mode the failed subtree can never complete; measure
            // progress against the reachable remainder.
            let excluded = if plan.failure_mode == am_proto::PlanFailureMode::Continue {
                gating_subtree_size(&failed_ids, &graph)
            } else {
                0
            };
            let attainable = (total - excluded).max(0);
            if total > 0 && attainable > 0 && completed >= attainable {
                if failed_ids.is_empty() {
                    self.finish_plan_run(
                        &plan_run_id,
                        WorkPlanRunState::Completed,
                        completed,
                        0,
                        0,
                        None,
                    )
                    .await;
                } else {
                    self.finish_plan_run(
                        &plan_run_id,
                        WorkPlanRunState::Failed,
                        completed,
                        0,
                        excluded,
                        Some(format!(
                            "completed all independent work; {} failed node(s) and their dependents were skipped",
                            failed_ids.len()
                        )),
                    )
                    .await;
                }
                return;
            }

            let mut started = 0i64;
            let ready_nodes = ready_runnable_nodes(&graph);
            let ready_count = ready_nodes.len();
            let start_budget = (plan.max_active_runs - active).max(0);
            if start_budget > 0 {
                for node in ready_nodes {
                    if started >= start_budget {
                        break;
                    }
                    let agent = node.primary_agent.unwrap_or(default_agent);
                    match self
                        .run_work_node_with_model_options(
                            &node.id,
                            agent,
                            permission,
                            execution_backend,
                            model_options.clone(),
                        )
                        .await
                    {
                        Ok(run_ref) => {
                            let _ = am_db::repos::work_graph::attach_run_to_plan(
                                &self.db.pool,
                                &run_ref,
                                &plan_run_id,
                            )
                            .await;
                            started += 1;
                        }
                        Err(err) if plan_start_error_is_retryable(&err.to_string()) => {}
                        Err(err) => {
                            self.finish_plan_run(
                                &plan_run_id,
                                WorkPlanRunState::Failed,
                                completed,
                                active,
                                blocked,
                                Some(err.to_string()),
                            )
                            .await;
                            return;
                        }
                    }
                }
            }

            // Continue-mode stall breaker: nothing running, nothing startable,
            // and the attainable remainder can't complete because failed
            // subtrees gate it — finish now instead of idling forever.
            if plan.failure_mode == am_proto::PlanFailureMode::Continue
                && !failed_ids.is_empty()
                && active == 0
                && started == 0
                && ready_count == 0
                && completed < attainable
            {
                self.finish_plan_run(
                    &plan_run_id,
                    WorkPlanRunState::Failed,
                    completed,
                    0,
                    blocked,
                    Some(format!(
                        "no runnable work remains; {} failed node(s) block the rest",
                        failed_ids.len()
                    )),
                )
                .await;
                return;
            }

            self.update_plan_run(
                &plan_run_id,
                WorkPlanRunState::Running,
                completed,
                active + started,
                blocked,
                None,
                false,
            )
            .await;
            // Event-driven: node/task/run state changes notify the waker; the
            // timeout is only a safety net against missed signals.
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            }
        }
    }

    /// Re-queue failed nodes that still have retry budget in this plan.
    /// Returns whether anything was requeued. The retry session resumes from
    /// the same worktree, whose TASK_CONTEXT.md carries the failure handoff.
    pub(crate) async fn retry_failed_plan_nodes(
        &self,
        plan: &WorkPlanRun,
        failed_ids: &[String],
    ) -> Result<bool, CoreError> {
        let mut retried = false;
        for node_id in failed_ids {
            let attempts = am_db::repos::work_graph::count_runs_for_node_in_plan(
                &self.db.pool,
                &plan.id,
                node_id,
            )
            .await?;
            // First run + N retries: attempts stays within max_node_retries + 1.
            if attempts > plan.max_node_retries {
                continue;
            }
            let node = self
                .update_work_node(
                    node_id,
                    WorkNodeUpdate {
                        status: Some(TaskStatus::Queued),
                        ..Default::default()
                    },
                )
                .await?;
            self.activity(
                Some(node.project_id.clone()),
                node.task_id.clone(),
                "work.node_retried",
                json!({
                    "node_id": node.id,
                    "plan_run_id": plan.id,
                    "attempt": attempts + 1,
                    "max_attempts": plan.max_node_retries + 1,
                }),
            )
            .await?;
            retried = true;
        }
        Ok(retried)
    }

    pub(crate) fn drive_work_plan_boxed(
        &self,
        plan_run_id: String,
        project_id: String,
        default_agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
        model_options: WorkRunModelOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.drive_work_plan(
            plan_run_id,
            project_id,
            default_agent,
            permission,
            execution_backend,
            model_options,
        ))
    }

    async fn apply_gate_mode(
        &self,
        plan: &WorkPlanRun,
        graph: &WorkGraph,
    ) -> Result<PlanGateOutcome, CoreError> {
        match plan.gate_mode {
            GateMode::Manual => {
                for node in &graph.nodes {
                    if node.kind == WorkNodeKind::Group {
                        continue;
                    }
                    if node.status == TaskStatus::Review {
                        let _ = am_db::repos::work_graph::set_plan_run_resume_after_node(
                            &self.db.pool,
                            &plan.id,
                            Some(&node.id),
                        )
                        .await;
                        return Ok(PlanGateOutcome::Paused);
                    }
                    if node.kind == WorkNodeKind::Milestone
                        && node.status != TaskStatus::Done
                        && prerequisites_complete(&node.id, graph)
                    {
                        let _ = self
                            .update_work_node(
                                &node.id,
                                WorkNodeUpdate {
                                    status: Some(TaskStatus::Review),
                                    ..Default::default()
                                },
                            )
                            .await?;
                        let _ = am_db::repos::work_graph::set_plan_run_resume_after_node(
                            &self.db.pool,
                            &plan.id,
                            Some(&node.id),
                        )
                        .await;
                        return Ok(PlanGateOutcome::Paused);
                    }
                }
                Ok(PlanGateOutcome::Continue)
            }
            GateMode::AutoEvaluate | GateMode::Autonomous => {
                let mut updated_any = false;
                for node in &graph.nodes {
                    if node.kind == WorkNodeKind::Group || is_active_status(node.status) {
                        continue;
                    }
                    let should_mark_done = node.status == TaskStatus::Review
                        || (node.kind == WorkNodeKind::Milestone
                            && node.status != TaskStatus::Done
                            && prerequisites_complete(&node.id, graph));
                    if should_mark_done {
                        if plan.gate_mode == GateMode::AutoEvaluate {
                            let agent = node
                                .primary_agent
                                .unwrap_or(plan.default_agent.unwrap_or(AgentKind::Codex));
                            let evaluation = self.evaluate_work_gate(plan, node, agent).await?;
                            match evaluation.verdict {
                                EvaluationVerdict::Pass => {
                                    let updated = self
                                        .update_work_node(
                                            &node.id,
                                            WorkNodeUpdate {
                                                status: Some(TaskStatus::Done),
                                                ..Default::default()
                                            },
                                        )
                                        .await?;
                                    self.activity(
                                        Some(updated.project_id.clone()),
                                        updated.task_id.clone(),
                                        "work.gate_auto_evaluated",
                                        json!({
                                            "node_id": updated.id,
                                            "plan_run_id": plan.id,
                                            "verdict": evaluation.verdict.as_str(),
                                        }),
                                    )
                                    .await?;
                                    updated_any = true;
                                }
                                EvaluationVerdict::Fail | EvaluationVerdict::NeedsHuman => {
                                    let status = if evaluation.verdict == EvaluationVerdict::Fail {
                                        TaskStatus::Paused
                                    } else {
                                        TaskStatus::Review
                                    };
                                    let _ = self
                                        .update_work_node(
                                            &node.id,
                                            WorkNodeUpdate {
                                                status: Some(status),
                                                ..Default::default()
                                            },
                                        )
                                        .await?;
                                    return Ok(PlanGateOutcome::Paused);
                                }
                            }
                        } else {
                            let updated = self
                                .update_work_node(
                                    &node.id,
                                    WorkNodeUpdate {
                                        status: Some(TaskStatus::Done),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                            self.activity(
                                Some(updated.project_id.clone()),
                                updated.task_id.clone(),
                                "work.gate_autonomous_passed",
                                json!({ "node_id": updated.id, "plan_run_id": plan.id }),
                            )
                            .await?;
                            updated_any = true;
                        }
                    }
                }
                Ok(if updated_any {
                    PlanGateOutcome::ContinueUpdated
                } else {
                    PlanGateOutcome::Continue
                })
            }
        }
    }

    async fn finish_plan_run(
        &self,
        plan_run_id: &str,
        state: WorkPlanRunState,
        completed_count: i64,
        active_count: i64,
        blocked_count: i64,
        error: Option<String>,
    ) {
        self.update_plan_run(
            plan_run_id,
            state,
            completed_count,
            active_count,
            blocked_count,
            error,
            true,
        )
        .await;
    }

    async fn update_plan_run(
        &self,
        plan_run_id: &str,
        state: WorkPlanRunState,
        completed_count: i64,
        active_count: i64,
        blocked_count: i64,
        error: Option<String>,
        ended: bool,
    ) {
        if let Ok(plan) = am_db::repos::work_graph::update_plan_run_progress(
            &self.db.pool,
            plan_run_id,
            state,
            completed_count,
            active_count,
            blocked_count,
            error.as_deref(),
            ended,
        )
        .await
        {
            self.events
                .publish(AppEvent::WorkPlanRunUpdated(plan.clone()));
            if ended {
                let _ = self
                    .activity(
                        Some(plan.project_id),
                        None,
                        "work.plan_ended",
                        json!({
                            "plan_run_id": plan.id,
                            "state": plan.state.as_str(),
                            "error": plan.error,
                        }),
                    )
                    .await;
            }
        }
    }

    pub(crate) async fn build_context_packet(
        &self,
        node: &WorkNode,
    ) -> Result<ContextPacket, CoreError> {
        let mut builder = ContextPacketBuilder::new(node.id.clone());
        builder.push(
            "work_node",
            Some(node.id.clone()),
            &node.title,
            &format!(
                "Status: {}\nPriority: {}\nObjective: {}",
                node.status.as_str(),
                node.priority.as_str(),
                node.description.as_deref().unwrap_or("None recorded.")
            ),
            "current work item",
            1.0,
        );

        if let Some(parent_id) = &node.parent_id {
            if let Some(parent) =
                am_db::repos::work_graph::get_node(&self.db.pool, parent_id).await?
            {
                builder.push(
                    "parent",
                    Some(parent.id),
                    &parent.title,
                    parent
                        .description
                        .as_deref()
                        .unwrap_or("No parent summary."),
                    "hierarchical parent context",
                    0.9,
                );
            }
        }

        let blockers =
            am_db::repos::work_graph::blocking_edges_for_node(&self.db.pool, &node.id).await?;
        for edge in blockers.into_iter().take(4) {
            let blocker_id = if edge.kind == am_proto::WorkEdgeKind::Blocks {
                edge.source_id
            } else {
                edge.target_id
            };
            if let Some(blocker) =
                am_db::repos::work_graph::get_node(&self.db.pool, &blocker_id).await?
            {
                builder.push(
                    "blocker",
                    Some(blocker.id),
                    &blocker.title,
                    blocker
                        .description
                        .as_deref()
                        .unwrap_or("No blocker summary."),
                    "unfinished dependency or blocker",
                    0.85,
                );
            }
        }

        // Completed prerequisites hand their outcomes forward: the dependent
        // session starts knowing what was built, where, and what's next —
        // and the files they touched boost ranking below.
        let mut boosts = ScoreBoosts::default();
        let predecessors =
            am_db::repos::work_graph::completed_gating_predecessors(&self.db.pool, &node.id)
                .await?;
        for predecessor in predecessors.into_iter().take(4) {
            let Some(task_id) = predecessor.task_id.as_deref() else {
                continue;
            };
            if let Some(handoff) =
                am_db::repos::task_context::latest_handoff(&self.db.pool, task_id).await?
            {
                boosts
                    .dependency_paths
                    .extend(handoff.changed_files.iter().cloned());
                builder.push(
                    "handoff",
                    Some(predecessor.id),
                    &format!("Prerequisite done: {}", predecessor.title),
                    &handoff.summary,
                    "handoff summary from a completed prerequisite",
                    0.85,
                );
            }
        }

        let repos = am_db::repos::work_graph::list_repo_bindings(&self.db.pool, &node.id).await?;
        let repo_ids = repos
            .iter()
            .map(|repo| repo.repo_id.clone())
            .collect::<Vec<_>>();
        for repo in &repos {
            builder.push(
                "repo",
                Some(repo.repo_id.clone()),
                &repo.repo_name,
                &format!(
                    "Branch: {}\nBase: {}\nWorkspace: {}",
                    repo.branch.as_deref().unwrap_or("not created yet"),
                    repo.base_ref.as_deref().unwrap_or("not created yet"),
                    repo.worktree_path.as_deref().unwrap_or("not created yet")
                ),
                "repository bound to this work item",
                0.8,
            );
        }
        self.refresh_context_index(&repos).await?;
        let denials = self.denied_context_globs_for_node(node, &repo_ids).await?;
        let indexed =
            am_db::repos::work_graph::list_repo_context_files(&self.db.pool, &repo_ids, 500)
                .await?;
        let candidates = indexed
            .into_iter()
            .filter(|file| !denials.denies(&file.repo_id, &file.path))
            .collect::<Vec<_>>();
        let ranked = rank_context_files(
            &node.title,
            node.description.as_deref(),
            candidates,
            &boosts,
            now(),
        );
        for (score, file) in ranked.into_iter().take(18) {
            builder.push(
                "repo_file",
                Some(file.repo_id),
                &file.path,
                &file.summary,
                "policy-filtered multi-repo source context",
                score,
            );
        }

        // What parallel sessions in the same plan run are doing, so
        // coordinated agents don't collide or duplicate work.
        for sibling in self.active_plan_siblings(node).await?.into_iter().take(4) {
            builder.push(
                "sibling",
                Some(sibling.node_id),
                &format!("In progress elsewhere: {}", sibling.title),
                &sibling.snippet,
                "parallel session in the same plan run",
                0.6,
            );
        }

        if let Ok(memories) =
            am_db::repos::memory::list_for_project(&self.db.pool, &node.project_id).await
        {
            for memory in memories.into_iter().take(5) {
                builder.push(
                    "memory",
                    Some(memory.id),
                    "Project memory",
                    &memory.body,
                    "project-level memory",
                    0.72,
                );
            }
        }
        if let Some(task_id) = &node.task_id {
            if let Ok(memories) = am_db::repos::memory::list_for_task(&self.db.pool, task_id).await
            {
                for memory in memories.into_iter().take(5) {
                    builder.push(
                        "memory",
                        Some(memory.id),
                        "Task memory",
                        &memory.body,
                        "task-level memory",
                        0.78,
                    );
                }
            }
        }
        if let Ok(docs) =
            am_db::repos::knowledge::list_for_project(&self.db.pool, &node.project_id).await
        {
            for doc in docs.into_iter().take(4) {
                builder.push(
                    "doc",
                    Some(doc.id),
                    &doc.title,
                    &doc.body,
                    "project knowledge document",
                    0.62,
                );
            }
        }
        if let Ok(hits) =
            am_db::repos::search::search(&self.db.pool, &node.title, Some(&node.project_id), 6)
                .await
        {
            for hit in hits {
                if hit.entity_id == node.id || hit.task_id.as_deref() == node.task_id.as_deref() {
                    continue;
                }
                builder.push(
                    &format!("search:{}", hit.kind),
                    Some(hit.entity_id),
                    &hit.title,
                    &hit.snippet,
                    "full-text match for this work title",
                    0.55,
                );
            }
        }

        Ok(builder.finish())
    }

    /// Titles and latest-handoff excerpts of other nodes actively running in
    /// the same plan run(s) as `node`.
    async fn active_plan_siblings(
        &self,
        node: &WorkNode,
    ) -> Result<Vec<SiblingContext>, CoreError> {
        let plans =
            am_db::repos::work_graph::list_plan_runs(&self.db.pool, &node.project_id).await?;
        let mut siblings = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for plan in plans
            .into_iter()
            .filter(|plan| plan.state == WorkPlanRunState::Running)
        {
            let runs =
                am_db::repos::work_graph::list_running_runs_for_plan(&self.db.pool, &plan.id)
                    .await?;
            // Only relevant when this node participates in the plan.
            if !runs.iter().any(|run| run.node_id == node.id) {
                continue;
            }
            for run in runs {
                if run.node_id == node.id || !seen.insert(run.node_id.clone()) {
                    continue;
                }
                let Some(sibling) =
                    am_db::repos::work_graph::get_node(&self.db.pool, &run.node_id).await?
                else {
                    continue;
                };
                let mut snippet = format!(
                    "Status: {}\nAgent: {}",
                    sibling.status.as_str(),
                    run.agent_kind.label()
                );
                if let Some(task_id) = sibling.task_id.as_deref() {
                    if let Some(handoff) =
                        am_db::repos::task_context::latest_handoff(&self.db.pool, task_id).await?
                    {
                        snippet.push_str("\nLast handoff:\n");
                        snippet.push_str(&truncate(&handoff.summary, 500));
                    }
                }
                siblings.push(SiblingContext {
                    node_id: sibling.id,
                    title: sibling.title,
                    snippet,
                });
            }
        }
        Ok(siblings)
    }

    /// Context-denial globs applying to `node`, split into globally-applied
    /// globs and per-repo globs — a rule scoped to repo A must not censor
    /// files from repo B in a multi-repo packet.
    async fn denied_context_globs_for_node(
        &self,
        node: &WorkNode,
        repo_ids: &[String],
    ) -> Result<ContextDenials, CoreError> {
        let _ = (node, repo_ids);
        Ok(ContextDenials::default())
    }

    async fn validate_gating_edge_candidate(
        &self,
        project_id: &str,
        replacing_edge_id: Option<&str>,
        source_id: &str,
        target_id: &str,
        kind: WorkEdgeKind,
    ) -> Result<(), CoreError> {
        let graph = am_db::repos::work_graph::graph(&self.db.pool, project_id).await?;
        let mut edges: Vec<WorkEdge> = graph
            .edges
            .into_iter()
            .filter(|edge| replacing_edge_id != Some(edge.id.as_str()))
            .collect();
        edges.push(WorkEdge {
            id: replacing_edge_id.unwrap_or("__candidate__").to_string(),
            project_id: project_id.to_string(),
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            kind,
            label: None,
            created_at: now(),
            updated_at: now(),
        });
        validate_gating_edges_acyclic(&graph.nodes, &edges)
    }
}

fn gating_dependency(edge: &WorkEdge) -> Option<(&str, &str)> {
    match edge.kind {
        WorkEdgeKind::DependsOn => Some((&edge.target_id, &edge.source_id)),
        WorkEdgeKind::Blocks | WorkEdgeKind::Handoff => Some((&edge.source_id, &edge.target_id)),
        WorkEdgeKind::SharesContext | WorkEdgeKind::RelatesTo => None,
    }
}

/// Size of the gating closure rooted at `roots`: the roots plus every node
/// transitively gated on them (unless already done). Used by continue-mode
/// plans to measure the attainable remainder.
fn gating_subtree_size(roots: &[String], graph: &WorkGraph) -> i64 {
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if let Some((prerequisite, dependent)) = gating_dependency(edge) {
            dependents.entry(prerequisite).or_default().push(dependent);
        }
    }
    let done: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|node| is_success_status(node.status))
        .map(|node| node.id.as_str())
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = roots.iter().map(String::as_str).collect();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        for next in dependents.get(id).into_iter().flatten() {
            // A dependent that already finished before the failure isn't lost.
            if !done.contains(next) {
                queue.push_back(next);
            }
        }
    }
    let group_ids: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == WorkNodeKind::Group)
        .map(|node| node.id.as_str())
        .collect();
    seen.iter().filter(|id| !group_ids.contains(*id)).count() as i64
}

fn validate_gating_edges_acyclic(nodes: &[WorkNode], edges: &[WorkEdge]) -> Result<(), CoreError> {
    let node_ids: HashSet<_> = nodes.iter().map(|node| node.id.as_str()).collect();
    let mut outgoing: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = nodes
        .iter()
        .map(|node| (node.id.as_str(), 0usize))
        .collect();

    for edge in edges {
        let Some((from, to)) = gating_dependency(edge) else {
            continue;
        };
        if from == to {
            return Err(CoreError::Other(
                "a gating work link cannot target itself".into(),
            ));
        }
        if !node_ids.contains(from) || !node_ids.contains(to) {
            continue;
        }
        let inserted = outgoing.entry(from).or_default().insert(to);
        if inserted {
            *indegree.entry(to).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop_front() {
        visited += 1;
        for next in outgoing.get(id).into_iter().flatten() {
            let count = indegree.entry(next).or_insert(0);
            *count = count.saturating_sub(1);
            if *count == 0 {
                queue.push_back(next);
            }
        }
    }

    if visited == nodes.len() {
        Ok(())
    } else {
        Err(CoreError::Other(
            "gating work links must not create dependency cycles".into(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanGateOutcome {
    /// Gate logic made no changes; the already-loaded graph is still current.
    Continue,
    /// Gate logic mutated node statuses (auto-passed milestones); the driver
    /// must reload the graph before computing counts.
    ContinueUpdated,
    Paused,
}

fn plan_counts(graph: &WorkGraph) -> (i64, i64, i64) {
    let mut completed = 0;
    let mut active = 0;
    let mut blocked = 0;
    for node in graph
        .nodes
        .iter()
        .filter(|node| node.kind != WorkNodeKind::Group)
    {
        if is_success_status(node.status) {
            completed += 1;
        } else if is_active_status(node.status) {
            active += 1;
        } else if !prerequisites_complete(&node.id, graph) {
            blocked += 1;
        }
    }
    (completed, active, blocked)
}

fn ready_runnable_nodes(graph: &WorkGraph) -> Vec<&WorkNode> {
    let mut nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, WorkNodeKind::Task | WorkNodeKind::Session))
        .filter(|node| {
            matches!(
                node.status,
                TaskStatus::Draft | TaskStatus::Queued | TaskStatus::Paused
            )
        })
        .filter(|node| prerequisites_complete(&node.id, graph))
        .collect();
    nodes.sort_by(|a, b| {
        priority_rank(b.priority)
            .cmp(&priority_rank(a.priority))
            .then(a.sort_order.cmp(&b.sort_order))
            .then(a.created_at.cmp(&b.created_at))
    });
    nodes
}

fn manual_gate_pending(graph: &WorkGraph) -> bool {
    graph.nodes.iter().any(|node| {
        node.kind != WorkNodeKind::Group
            && (node.status == TaskStatus::Review
                || (node.kind == WorkNodeKind::Milestone
                    && node.status != TaskStatus::Done
                    && prerequisites_complete(&node.id, graph)))
    })
}

fn prerequisites_complete(node_id: &str, graph: &WorkGraph) -> bool {
    let nodes_by_id: HashMap<_, _> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    graph
        .edges
        .iter()
        .filter_map(gating_dependency)
        .filter(|(_, dependent)| *dependent == node_id)
        .all(|(prereq, _)| {
            nodes_by_id
                .get(prereq)
                .is_some_and(|node| is_success_status(node.status))
        })
}

fn is_success_status(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Cancelled)
}

fn is_active_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Running
            | TaskStatus::AwaitingApproval
            | TaskStatus::WaitingForLimit
            | TaskStatus::WaitingForNetwork
    )
}

fn priority_rank(priority: am_proto::TaskPriority) -> u8 {
    match priority {
        am_proto::TaskPriority::Urgent => 3,
        am_proto::TaskPriority::High => 2,
        am_proto::TaskPriority::Medium => 1,
        am_proto::TaskPriority::Low => 0,
    }
}

fn plan_start_error_is_retryable(error: &str) -> bool {
    error.contains("already running")
        || error.contains("maximum concurrent")
        || error.contains("repository is already locked")
}

/// A parallel session's identity and latest progress, for packet inclusion.
struct SiblingContext {
    node_id: String,
    title: String,
    snippet: String,
}

/// Denied context globs split by applicability.
#[derive(Default)]
struct ContextDenials {
    global: Vec<String>,
    per_repo: HashMap<String, Vec<String>>,
}

impl ContextDenials {
    fn denies(&self, repo_id: &str, path: &str) -> bool {
        path_denied(path, &self.global)
            || self
                .per_repo
                .get(repo_id)
                .is_some_and(|globs| path_denied(path, globs))
    }
}

fn path_denied(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|glob| wildcard_path(glob, path))
}

fn wildcard_path(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == "**/*" {
        return true;
    }
    let pattern = pattern.trim_start_matches("**/");
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    value == pattern || value.ends_with(pattern)
}

struct ContextPacketBuilder {
    node_id: String,
    used_bytes: i64,
    used_tokens: i64,
    tokens_by_source: HashMap<String, i64>,
    inclusions: Vec<ContextInclusion>,
}

impl ContextPacketBuilder {
    fn new(node_id: String) -> Self {
        Self {
            node_id,
            used_bytes: 0,
            used_tokens: 0,
            tokens_by_source: HashMap::new(),
            inclusions: Vec::new(),
        }
    }

    fn push(
        &mut self,
        source_kind: &str,
        entity_id: Option<String>,
        title: &str,
        snippet: &str,
        reason: &str,
        score: f64,
    ) {
        let snippet = truncate(snippet, 1_400);
        let title = truncate(title, 180);
        let bytes = (source_kind.len() + title.len() + snippet.len() + reason.len()) as i64;
        let estimated_tokens = (bytes / 4).max(1);
        if self.used_bytes + bytes > CONTEXT_HARD_CAP_BYTES
            || self.used_tokens + estimated_tokens > CONTEXT_HARD_CAP_TOKENS
        {
            return;
        }
        let over_soft_budget =
            self.used_bytes >= CONTEXT_BUDGET_BYTES || self.used_tokens >= CONTEXT_BUDGET_TOKENS;
        if over_soft_budget && score < 0.8 {
            return;
        }
        // Per-source ceiling: one noisy source (many files, long memories)
        // can't crowd out every other kind of context.
        let source_used = self.tokens_by_source.get(source_kind).copied().unwrap_or(0);
        if source_used + estimated_tokens > source_token_budget(source_kind) {
            return;
        }
        self.used_bytes += bytes;
        self.used_tokens += estimated_tokens;
        *self
            .tokens_by_source
            .entry(source_kind.to_string())
            .or_insert(0) += estimated_tokens;
        self.inclusions.push(ContextInclusion {
            source_kind: source_kind.to_string(),
            entity_id,
            title,
            snippet,
            reason: reason.to_string(),
            score,
            bytes,
            estimated_tokens,
        });
    }

    fn finish(self) -> ContextPacket {
        let mut summary = String::new();
        summary.push_str("AgentManager selected a bounded context packet for this run.\n");
        summary.push_str(
            "Use these inclusions as orientation; ask or search when more detail is needed.\n",
        );
        for inclusion in &self.inclusions {
            summary.push_str(&format!(
                "\n- [{}] {} - {}",
                inclusion.source_kind, inclusion.title, inclusion.reason
            ));
        }
        ContextPacket {
            id: new_id(),
            node_id: self.node_id,
            budget_bytes: CONTEXT_BUDGET_BYTES,
            used_bytes: self.used_bytes,
            summary,
            inclusions: self.inclusions,
            created_at: now(),
        }
    }
}

fn render_context_packet_prompt(packet: &ContextPacket) -> String {
    let mut out = String::new();
    out.push_str("Use this AgentManager context packet. It is intentionally bounded; do not assume omitted files or transcript history are irrelevant if you discover you need them.\n\n");
    let used_tokens: i64 = packet
        .inclusions
        .iter()
        .map(|inclusion| inclusion.estimated_tokens)
        .sum();
    out.push_str(&format!(
        "Context budget: {}/{} bytes (~{} tokens) used.\n\n",
        packet.used_bytes, packet.budget_bytes, used_tokens
    ));
    out.push_str("## Selected Context\n");
    for inclusion in &packet.inclusions {
        out.push_str(&format!(
            "\n### {}: {}\nReason: {}\n{}\n",
            inclusion.source_kind, inclusion.title, inclusion.reason, inclusion.snippet
        ));
    }
    out
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn push_n(builder: &mut ContextPacketBuilder, source: &str, n: usize, score: f64) {
        let filler = "x".repeat(1_200);
        for i in 0..n {
            builder.push(source, None, &format!("item-{i}"), &filler, "test", score);
        }
    }

    #[test]
    fn per_source_token_ceiling_leaves_room_for_other_sources() {
        let mut builder = ContextPacketBuilder::new("node".into());
        // Each filler item is ~300 tokens; repo_file caps at 3000.
        push_n(&mut builder, "repo_file", 30, 0.9);
        let repo_tokens: i64 = builder
            .inclusions
            .iter()
            .filter(|inclusion| inclusion.source_kind == "repo_file")
            .map(|inclusion| inclusion.estimated_tokens)
            .sum();
        assert!(
            repo_tokens <= 3_000,
            "repo files exceeded ceiling: {repo_tokens}"
        );

        // Memories still fit after files saturated their category.
        push_n(&mut builder, "memory", 2, 0.9);
        assert!(builder
            .inclusions
            .iter()
            .any(|inclusion| inclusion.source_kind == "memory"));
    }

    #[test]
    fn hard_token_cap_is_absolute_even_for_high_scores() {
        let mut builder = ContextPacketBuilder::new("node".into());
        push_n(&mut builder, "work_node", 100, 1.0); // uncapped category
        let total: i64 = builder
            .inclusions
            .iter()
            .map(|inclusion| inclusion.estimated_tokens)
            .sum();
        assert!(total <= CONTEXT_HARD_CAP_TOKENS);
    }

    #[test]
    fn low_scores_are_rejected_past_the_soft_budget() {
        let mut builder = ContextPacketBuilder::new("node".into());
        push_n(&mut builder, "work_node", 100, 0.79);
        let total: i64 = builder
            .inclusions
            .iter()
            .map(|inclusion| inclusion.estimated_tokens)
            .sum();
        assert!(
            total <= CONTEXT_BUDGET_TOKENS + 400,
            "soft budget ignored: {total}"
        );
    }

    #[test]
    fn per_repo_denials_do_not_censor_other_repos() {
        let mut denials = ContextDenials::default();
        denials
            .per_repo
            .insert("repo-a".into(), vec!["*.env".into()]);
        denials.global.push("secrets/*".into());

        assert!(denials.denies("repo-a", "config/prod.env"));
        assert!(
            !denials.denies("repo-b", "config/prod.env"),
            "repo-scoped rule leaked"
        );
        assert!(
            denials.denies("repo-b", "secrets/key.pem"),
            "global rule applies everywhere"
        );
    }
}

pub(crate) fn truncate(value: &str, max: usize) -> String {
    let value = value.trim();
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max.saturating_sub(20);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... [trimmed]", &value[..end])
}
