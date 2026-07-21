//! Orchestrator core — the UI-agnostic heart of Perpetual.
//!
//! Holds the database, the event bus, the agent registry, and the session
//! manager, plus the service methods the Tauri shell (today) or a headless
//! daemon (M7) drives. It has **no** Tauri dependency by design.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use am_agents::PermissionPolicy;
use am_db::Db;
use am_proto::{
    ActivityEvent, AgentKind, AppEvent, KnowledgeDoc, KnowledgeDocUpdate, MemoryNote,
    MemoryNoteUpdate, NewActivity, NewKnowledgeDoc, NewMemoryNote, NewProject, NewTask, Project,
    SearchHit, Task, TaskStatus, TaskUpdate,
};
use serde_json::json;
use tokio::sync::Mutex;

/// A user follow-up queued while a task's session is running. Drained (run) when
/// the current session ends.
#[derive(Debug, Clone)]
pub(crate) struct QueuedMessage {
    pub agent: AgentKind,
    pub permission: PermissionPolicy,
    pub message: String,
}

type MessageQueues = Arc<Mutex<HashMap<String, VecDeque<QueuedMessage>>>>;

mod admission;
mod agent_thread;
mod agents;
mod approvals;
mod availability;
mod budget;
mod bus;
mod capacity;
mod cloud_handoff;
mod context_index;
mod context_scoring;
mod error;
mod evaluator;
mod fallback;
mod github;
mod layout;
mod local_models;
mod network;
mod orchestrate;
mod policy;
mod sandbox;
mod scheduler;
mod session_manager;
mod task_context;
mod work_graph;

pub use agents::AgentRegistry;
pub(crate) use approvals::ApprovalScope;
use approvals::{new_registry, ApprovalRegistry};
pub use bus::EventBus;
pub use cloud_handoff::PowerEvent;
pub use error::CoreError;
use sandbox::SandboxManager;
use scheduler::Scheduler;
pub use session_manager::{SessionManager, SessionPermit};
pub use work_graph::WorkRunModelOptions;

/// Maximum number of agent sessions running concurrently. Bounds CPU/RAM and
/// the number of spawned processes.
pub const MAX_CONCURRENT_SESSIONS: usize = 4;
const MAX_RUNTIME_SESSION_PERMITS: usize = 512;

pub fn default_max_concurrent_sessions() -> usize {
    std::thread::available_parallelism()
        .map(|cpus| cpus.get().saturating_sub(1).clamp(2, 8))
        .unwrap_or(MAX_CONCURRENT_SESSIONS)
}

/// The application core: shared, cheaply-cloneable handle to all services.
#[derive(Clone)]
pub struct AppCore {
    pub db: Db,
    pub events: EventBus,
    data_dir: PathBuf,
    agents: Arc<AgentRegistry>,
    sessions: Arc<SessionManager>,
    sandboxes: Arc<SandboxManager>,
    scheduler: Arc<Scheduler>,
    messages: MessageQueues,
    approvals: ApprovalRegistry,
    /// Short-TTL cache of CPU/memory readings (session-start hot path).
    capacity_cache: Arc<std::sync::Mutex<Option<(Instant, capacity::LocalSystemCapacity)>>>,
    /// Wakes the scheduler loop early (session ended, work queued, limit
    /// marked) so continuations start immediately instead of on the next tick.
    scheduler_wake: Arc<tokio::sync::Notify>,
    /// Earliest known provider-limit reset time; the scheduler sleeps until
    /// exactly then rather than polling every interval.
    next_reset_deadline: Arc<std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Per-project wakers for plan-run drivers: node/task/run state changes
    /// notify the project's driver instead of it polling on a fixed cadence.
    plan_wakers: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Notify>>>>,
    /// Debounce handles for automatic graph layout, one per project.
    layout_debounce: Arc<std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Last background-checkpoint time per running thread (cloud continuity).
    cloud_checkpoint_marks: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    /// Last provider/remote poll per active cloud run.
    cloud_monitor_marks: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    /// Serialize cloud handoffs so a power event, manual action, and daemon
    /// shutdown cannot all checkpoint and launch the same thread concurrently.
    cloud_handoff_lock: Arc<tokio::sync::Mutex<()>>,
}

const CANCEL_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

impl AppCore {
    /// Initialize the core: open the database under `data_dir` and set up the
    /// agent registry and session manager. Worktrees live under `data_dir`.
    pub async fn new(data_dir: &Path) -> Result<Self, CoreError> {
        std::fs::create_dir_all(data_dir).ok();
        let db = Db::connect(&data_dir.join("perpetual.db")).await?;

        // Reconcile state left over from a previous process: no agent process
        // survives a restart, so any `running` session/task is stale.
        am_db::repos::session::mark_orphans_interrupted(&db.pool)
            .await
            .ok();
        am_db::repos::agent_turn::mark_orphans_interrupted(&db.pool)
            .await
            .ok();
        am_db::repos::task::pause_orphaned_running(&db.pool)
            .await
            .ok();
        am_db::repos::agent_thread::pause_orphaned_running(&db.pool)
            .await
            .ok();

        let core = Self {
            db,
            events: EventBus::new(),
            data_dir: data_dir.to_path_buf(),
            agents: Arc::new(AgentRegistry::new()),
            sessions: Arc::new(SessionManager::new(MAX_RUNTIME_SESSION_PERMITS)),
            sandboxes: Arc::new(SandboxManager::new(8)),
            scheduler: Arc::new(Scheduler::new()),
            messages: Arc::new(Mutex::new(HashMap::new())),
            approvals: new_registry(),
            capacity_cache: Arc::new(std::sync::Mutex::new(None)),
            scheduler_wake: Arc::new(tokio::sync::Notify::new()),
            next_reset_deadline: Arc::new(std::sync::Mutex::new(None)),
            plan_wakers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            layout_debounce: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cloud_checkpoint_marks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cloud_monitor_marks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cloud_handoff_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        core.reconcile_stale_sandboxes();
        core.scheduler.start(core.clone_for_scheduler()).await;
        core.wake_scheduler();
        Ok(core)
    }

    /// Cancel all running sessions (call on app shutdown).
    pub async fn shutdown(&self) {
        self.scheduler.shutdown().await;
        self.sessions.shutdown().await;
        // Sweep any app-owned sandboxes that outlived their session cancellation.
        let _ = tokio::task::spawn_blocking(sandbox::reconcile_owned_sandboxes).await;
    }

    fn clone_for_scheduler(&self) -> Self {
        Self {
            db: self.db.clone(),
            events: self.events.clone(),
            data_dir: self.data_dir.clone(),
            agents: self.agents.clone(),
            sessions: self.sessions.clone(),
            sandboxes: self.sandboxes.clone(),
            scheduler: Arc::new(Scheduler::new()),
            messages: self.messages.clone(),
            approvals: self.approvals.clone(),
            capacity_cache: self.capacity_cache.clone(),
            scheduler_wake: self.scheduler_wake.clone(),
            next_reset_deadline: self.next_reset_deadline.clone(),
            plan_wakers: self.plan_wakers.clone(),
            layout_debounce: self.layout_debounce.clone(),
            cloud_checkpoint_marks: self.cloud_checkpoint_marks.clone(),
            cloud_monitor_marks: self.cloud_monitor_marks.clone(),
            cloud_handoff_lock: self.cloud_handoff_lock.clone(),
        }
    }

    /// Record an activity entry and broadcast it live.
    async fn activity(
        &self,
        project_id: Option<String>,
        task_id: Option<String>,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), CoreError> {
        let event = am_db::repos::event::record(
            &self.db.pool,
            NewActivity {
                project_id,
                task_id,
                kind: kind.to_string(),
                payload,
            },
        )
        .await?;
        self.events.publish(AppEvent::Activity(event));
        Ok(())
    }

    // ---- Projects -------------------------------------------------------

    pub async fn create_project(&self, input: NewProject) -> Result<Project, CoreError> {
        let project = am_db::repos::project::create(&self.db.pool, input).await?;
        self.events
            .publish(AppEvent::ProjectCreated(project.clone()));
        self.activity(
            Some(project.id.clone()),
            None,
            "project.created",
            json!({ "name": project.name }),
        )
        .await?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, CoreError> {
        Ok(am_db::repos::project::list(&self.db.pool).await?)
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>, CoreError> {
        Ok(am_db::repos::project::get(&self.db.pool, id).await?)
    }

    /// Delete a project and everything in it. Tasks, repos, work nodes/edges,
    /// docs, memory, and activity cascade with the project row; agent threads
    /// only SET NULL, so we delete them explicitly — which also force-stops any
    /// active run and tears down its sandbox. Running tasks are cancelled first.
    pub async fn delete_project(&self, project_id: &str) -> Result<(), CoreError> {
        let project = am_db::repos::project::get(&self.db.pool, project_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let threads = am_db::repos::agent_thread::list(&self.db.pool, Some(project_id)).await?;
        for thread in threads {
            // Do not delete the project underneath a session that failed to
            // settle. The caller must be able to retry once the agent process
            // has exited, otherwise its consumer can keep writing into a
            // project whose rows have just been cascaded away.
            self.delete_agent_thread(&thread.id, true).await?;
        }
        let tasks = am_db::repos::task::list_for_project(&self.db.pool, project_id).await?;
        for task in tasks {
            if self.sessions.is_active(&task.id).await {
                let _ = self.sessions.cancel(&task.id).await;
                if !self
                    .sessions
                    .wait_until_inactive(&task.id, CANCEL_SETTLE)
                    .await
                {
                    return Err(CoreError::Other(
                        "a running task did not stop before project deletion timed out".into(),
                    ));
                }
            }
        }
        am_db::repos::project::delete(&self.db.pool, project_id).await?;
        self.activity(
            None,
            None,
            "project.deleted",
            json!({ "project_id": project_id, "name": project.name }),
        )
        .await?;
        Ok(())
    }

    // ---- Tasks ----------------------------------------------------------

    pub async fn create_task(&self, input: NewTask) -> Result<Task, CoreError> {
        let repo_id = input.repo_id.clone().filter(|id| !id.trim().is_empty());
        if let Some(repo_id) = &repo_id {
            self.validate_project_repo(&input.project_id, repo_id)
                .await?;
        }

        let task = am_db::repos::task::create(&self.db.pool, input).await?;
        if let Some(repo_id) = repo_id {
            am_db::repos::task_repo::replace_repo(&self.db.pool, &task.id, &repo_id).await?;
        }
        self.ensure_task_context(&task).await?;
        self.events.publish(AppEvent::TaskCreated(task.clone()));
        self.activity(
            Some(task.project_id.clone()),
            Some(task.id.clone()),
            "task.created",
            json!({ "title": task.title }),
        )
        .await?;
        Ok(task)
    }

    pub async fn list_tasks(&self, project_id: &str) -> Result<Vec<Task>, CoreError> {
        Ok(am_db::repos::task::list_for_project(&self.db.pool, project_id).await?)
    }

    pub async fn get_task(&self, id: &str) -> Result<Option<Task>, CoreError> {
        Ok(am_db::repos::task::get(&self.db.pool, id).await?)
    }

    pub async fn update_task(&self, id: &str, patch: TaskUpdate) -> Result<Task, CoreError> {
        let before = am_db::repos::task::get(&self.db.pool, id).await?;
        let task = am_db::repos::task::update(&self.db.pool, id, patch).await?;
        if let Some(before) = before {
            self.sync_task_context_from_task_update(&before, &task)
                .await?;
        } else {
            self.ensure_task_context(&task).await?;
        }
        self.events.publish(AppEvent::TaskUpdated(task.clone()));
        // Plan drivers watch task-backed node statuses; queued work should
        // start on the next scheduler pass, not the next 30s tick.
        self.notify_plan_watchers(&task.project_id);
        if task.status == TaskStatus::Queued {
            self.wake_scheduler();
        }
        self.activity(
            Some(task.project_id.clone()),
            Some(task.id.clone()),
            "task.updated",
            json!({ "status": task.status.as_str() }),
        )
        .await?;
        if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled)
            && !self.sessions.is_active(&task.id).await
        {
            self.cleanup_task_sandboxes(&task.id).await;
        }
        Ok(task)
    }

    // ---- Knowledge ------------------------------------------------------

    pub async fn create_knowledge_doc(
        &self,
        input: NewKnowledgeDoc,
    ) -> Result<KnowledgeDoc, CoreError> {
        let doc = am_db::repos::knowledge::create(&self.db.pool, input).await?;
        self.activity(
            Some(doc.project_id.clone()),
            None,
            "knowledge.created",
            json!({ "id": doc.id, "title": doc.title }),
        )
        .await?;
        Ok(doc)
    }

    pub async fn list_knowledge_docs(
        &self,
        project_id: &str,
    ) -> Result<Vec<KnowledgeDoc>, CoreError> {
        Ok(am_db::repos::knowledge::list_for_project(&self.db.pool, project_id).await?)
    }

    pub async fn get_knowledge_doc(&self, id: &str) -> Result<Option<KnowledgeDoc>, CoreError> {
        Ok(am_db::repos::knowledge::get(&self.db.pool, id).await?)
    }

    pub async fn update_knowledge_doc(
        &self,
        id: &str,
        patch: KnowledgeDocUpdate,
    ) -> Result<KnowledgeDoc, CoreError> {
        let doc = am_db::repos::knowledge::update(&self.db.pool, id, patch).await?;
        self.activity(
            Some(doc.project_id.clone()),
            None,
            "knowledge.updated",
            json!({ "id": doc.id, "title": doc.title }),
        )
        .await?;
        Ok(doc)
    }

    pub async fn delete_knowledge_doc(&self, id: &str) -> Result<(), CoreError> {
        let doc = am_db::repos::knowledge::get(&self.db.pool, id).await?;
        am_db::repos::knowledge::delete(&self.db.pool, id).await?;
        if let Some(doc) = doc {
            self.activity(
                Some(doc.project_id),
                None,
                "knowledge.deleted",
                json!({ "id": doc.id, "title": doc.title }),
            )
            .await?;
        }
        Ok(())
    }

    // ---- Memory ---------------------------------------------------------

    pub async fn create_memory_note(&self, input: NewMemoryNote) -> Result<MemoryNote, CoreError> {
        let task_id = input.task_id.clone();
        let note = am_db::repos::memory::create(&self.db.pool, input).await?;
        self.activity(
            Some(note.project_id.clone()),
            task_id,
            "memory.created",
            json!({ "id": note.id }),
        )
        .await?;
        Ok(note)
    }

    pub async fn list_project_memory(
        &self,
        project_id: &str,
    ) -> Result<Vec<MemoryNote>, CoreError> {
        Ok(am_db::repos::memory::list_for_project(&self.db.pool, project_id).await?)
    }

    pub async fn list_task_memory(&self, task_id: &str) -> Result<Vec<MemoryNote>, CoreError> {
        Ok(am_db::repos::memory::list_for_task(&self.db.pool, task_id).await?)
    }

    pub async fn update_memory_note(
        &self,
        id: &str,
        patch: MemoryNoteUpdate,
    ) -> Result<MemoryNote, CoreError> {
        let note = am_db::repos::memory::update(&self.db.pool, id, patch).await?;
        self.activity(
            Some(note.project_id.clone()),
            note.task_id.clone(),
            "memory.updated",
            json!({ "id": note.id }),
        )
        .await?;
        Ok(note)
    }

    pub async fn delete_memory_note(&self, id: &str) -> Result<(), CoreError> {
        let note = am_db::repos::memory::get(&self.db.pool, id).await?;
        am_db::repos::memory::delete(&self.db.pool, id).await?;
        if let Some(note) = note {
            self.activity(
                Some(note.project_id),
                note.task_id,
                "memory.deleted",
                json!({ "id": note.id }),
            )
            .await?;
        }
        Ok(())
    }

    // ---- Activity -------------------------------------------------------

    pub async fn list_activity(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ActivityEvent>, CoreError> {
        Ok(match project_id {
            Some(pid) => am_db::repos::event::list_for_project(&self.db.pool, pid, limit).await?,
            None => am_db::repos::event::list_recent(&self.db.pool, limit).await?,
        })
    }

    // ---- Search ---------------------------------------------------------

    /// Full-text search across tasks, docs, and memory. `project_id = None`
    /// searches every project.
    pub async fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SearchHit>, CoreError> {
        Ok(am_db::repos::search::search(&self.db.pool, query, project_id, limit).await?)
    }
}

/// A fully-wired core over an in-memory database, for tests anywhere in the
/// crate (only `lib.rs` can construct `AppCore` literally — fields are
/// module-private).
#[cfg(test)]
pub(crate) async fn test_core() -> AppCore {
    let db = Db::connect_in_memory().await.unwrap();
    AppCore {
        db,
        events: EventBus::new(),
        data_dir: std::env::temp_dir().join("perpetual-test"),
        agents: Arc::new(AgentRegistry::new()),
        sessions: Arc::new(SessionManager::new(MAX_RUNTIME_SESSION_PERMITS)),
        sandboxes: Arc::new(SandboxManager::new(8)),
        scheduler: Arc::new(Scheduler::new()),
        messages: Arc::new(Mutex::new(HashMap::new())),
        approvals: new_registry(),
        capacity_cache: Arc::new(std::sync::Mutex::new(None)),
        scheduler_wake: Arc::new(tokio::sync::Notify::new()),
        next_reset_deadline: Arc::new(std::sync::Mutex::new(None)),
        plan_wakers: Arc::new(std::sync::Mutex::new(HashMap::new())),
        layout_debounce: Arc::new(std::sync::Mutex::new(HashMap::new())),
        cloud_checkpoint_marks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        cloud_monitor_marks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        cloud_handoff_lock: Arc::new(tokio::sync::Mutex::new(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_db::repos::{repo, task_repo};
    use am_proto::{
        GateMode, LayoutMode, NewWorkEdge, NewWorkNode, TaskPriority, WorkEdgeKind, WorkNodeKind,
        WorkNodeUpdate, WorkPlanRunState,
    };

    async fn core() -> AppCore {
        test_core().await
    }

    #[tokio::test]
    async fn project_task_roundtrip() {
        let core = core().await;
        let mut rx = core.events.subscribe();

        let project = core
            .create_project(NewProject {
                name: "Demo".into(),
                description: Some("a project".into()),
            })
            .await
            .unwrap();

        let projects = core.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Demo");

        let task = core
            .create_task(NewTask {
                project_id: project.id.clone(),
                title: "Build a feature".into(),
                repo_id: None,
                description: None,
                priority: TaskPriority::High,
                primary_agent: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let tasks = core.list_tasks(&project.id).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);

        let mut saw_project = false;
        let mut saw_task = false;
        for _ in 0..6 {
            match rx.try_recv().map(|sequenced| sequenced.event) {
                Ok(AppEvent::ProjectCreated(_)) => saw_project = true,
                Ok(AppEvent::TaskCreated(_)) => saw_task = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_project && saw_task);

        let activity = core.list_activity(Some(&project.id), 50).await.unwrap();
        assert!(activity.iter().any(|e| e.kind == "task.created"));
    }

    #[tokio::test]
    async fn work_graph_projects_task_repo_hierarchy_and_context() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Graph".into(),
                description: None,
            })
            .await
            .unwrap();
        let repo = repo::create_local(
            &core.db.pool,
            &project.id,
            "app",
            "/tmp/perpetual-app",
            "main",
        )
        .await
        .unwrap();

        let group = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Group),
                title: "Launch".into(),
                description: Some("Enterprise launch plan".into()),
                priority: TaskPriority::High,
                primary_agent: None,
                repo_ids: vec![],
                position_x: Some(10.0),
                position_y: Some(20.0),
                ..Default::default()
            })
            .await
            .unwrap();
        let blocker = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: Some(group.id.clone()),
                kind: Some(WorkNodeKind::Task),
                title: "Design API".into(),
                description: Some("Define API contracts".into()),
                priority: TaskPriority::Medium,
                primary_agent: Some(AgentKind::Codex),
                repo_ids: vec![repo.id.clone()],
                position_x: Some(80.0),
                position_y: Some(80.0),
                ..Default::default()
            })
            .await
            .unwrap();
        let task = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: Some(group.id.clone()),
                kind: Some(WorkNodeKind::Task),
                title: "Implement API".into(),
                description: Some("Build the API after design settles".into()),
                priority: TaskPriority::High,
                primary_agent: Some(AgentKind::Codex),
                repo_ids: vec![repo.id.clone()],
                position_x: Some(160.0),
                position_y: Some(120.0),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(task.task_id.is_some());
        let graph = core.get_work_graph(&project.id).await.unwrap();
        assert_eq!(graph.nodes.len(), 3);
        assert!(graph
            .repo_bindings
            .iter()
            .any(|binding| binding.node_id == task.id && binding.repo_id == repo.id));

        let moved = core
            .move_work_node(&task.id, None, 320.0, 240.0)
            .await
            .unwrap();
        assert!(moved.parent_id.is_none());
        assert_eq!(moved.position_x, 320.0);

        core.connect_work_nodes(NewWorkEdge {
            project_id: project.id.clone(),
            source_id: task.id.clone(),
            target_id: blocker.id.clone(),
            kind: WorkEdgeKind::DependsOn,
            label: None,
        })
        .await
        .unwrap();

        let packet = core.preview_context_packet(&task.id).await.unwrap();
        assert!(packet.used_bytes <= packet.budget_bytes);
        assert!(packet.inclusions.iter().any(|item| {
            item.source_kind == "blocker" && item.entity_id.as_deref() == Some(&blocker.id)
        }));

        let bindings = core
            .assign_work_node_repos(&task.id, Vec::new())
            .await
            .unwrap();
        assert!(bindings.is_empty());
        assert!(
            task_repo::get_for_task(&core.db.pool, task.task_id.as_deref().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn work_graph_layout_defaults_prettifies_and_rejects_cycles() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Layout".into(),
                description: None,
            })
            .await
            .unwrap();

        let a = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Task),
                title: "Design".into(),
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: Some(AgentKind::Codex),
                repo_ids: vec![],
                position_x: None,
                position_y: None,
                ..Default::default()
            })
            .await
            .unwrap();
        let b = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Task),
                title: "Build".into(),
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: Some(AgentKind::Codex),
                repo_ids: vec![],
                position_x: None,
                position_y: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_ne!(
            (a.position_x, a.position_y),
            (b.position_x, b.position_y),
            "nodes without coordinates should not stack"
        );

        let c = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Milestone),
                title: "Gate".into(),
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: None,
                repo_ids: vec![],
                position_x: None,
                position_y: None,
                ..Default::default()
            })
            .await
            .unwrap();

        core.connect_work_nodes(NewWorkEdge {
            project_id: project.id.clone(),
            source_id: b.id.clone(),
            target_id: a.id.clone(),
            kind: WorkEdgeKind::DependsOn,
            label: None,
        })
        .await
        .unwrap();
        core.connect_work_nodes(NewWorkEdge {
            project_id: project.id.clone(),
            source_id: c.id.clone(),
            target_id: b.id.clone(),
            kind: WorkEdgeKind::DependsOn,
            label: None,
        })
        .await
        .unwrap();

        let cycle = core
            .connect_work_nodes(NewWorkEdge {
                project_id: project.id.clone(),
                source_id: a.id.clone(),
                target_id: c.id.clone(),
                kind: WorkEdgeKind::DependsOn,
                label: None,
            })
            .await;
        assert!(cycle.is_err(), "gating links should remain acyclic");

        let pretty = core
            .prettify_work_graph(&project.id, LayoutMode::Force)
            .await
            .unwrap();
        let by_id: HashMap<_, _> = pretty.nodes.iter().map(|node| (&node.id, node)).collect();
        assert!(by_id[&a.id].position_x < by_id[&b.id].position_x);
        assert!(by_id[&b.id].position_x < by_id[&c.id].position_x);
    }

    #[tokio::test]
    async fn work_plan_runs_auto_gate_and_manual_pause_without_agents() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Plan".into(),
                description: None,
            })
            .await
            .unwrap();
        let milestone = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Milestone),
                title: "Launch gate".into(),
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: None,
                repo_ids: vec![],
                position_x: None,
                position_y: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let plan = core
            .run_work_plan(
                &project.id,
                GateMode::Autonomous,
                Some(1),
                AgentKind::Codex,
                PermissionPolicy::ReadOnly,
                None,
            )
            .await
            .unwrap();
        let completed = wait_for_plan_state(&core, &plan.id, WorkPlanRunState::Completed).await;
        assert_eq!(completed.completed_count, 1);

        core.update_work_node(
            &milestone.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Draft),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let manual = core
            .run_work_plan(
                &project.id,
                GateMode::Manual,
                Some(1),
                AgentKind::Codex,
                PermissionPolicy::ReadOnly,
                None,
            )
            .await
            .unwrap();
        let paused = wait_for_plan_state(&core, &manual.id, WorkPlanRunState::Paused).await;
        assert_eq!(paused.blocked_count, 0);
    }

    async fn wait_for_plan_state(
        core: &AppCore,
        plan_id: &str,
        state: WorkPlanRunState,
    ) -> am_proto::WorkPlanRun {
        for _ in 0..20 {
            if let Some(plan) = core.get_work_plan_run(plan_id).await.unwrap() {
                if plan.state == state {
                    return plan;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("plan {plan_id} did not reach {state:?}");
    }

    #[tokio::test]
    async fn continue_mode_plan_skips_failed_subtree_and_reports_it() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Continue".into(),
                description: None,
            })
            .await
            .unwrap();
        let task_node = |title: &str| NewWorkNode {
            project_id: project.id.clone(),
            parent_id: None,
            kind: Some(WorkNodeKind::Task),
            title: title.into(),
            description: None,
            priority: TaskPriority::Medium,
            primary_agent: Some(AgentKind::Codex),
            repo_ids: vec![],
            position_x: None,
            position_y: None,
            ..Default::default()
        };

        // One pre-failed task, one independent milestone that auto-passes.
        let failed = core
            .create_work_node(task_node("Flaky work"))
            .await
            .unwrap();
        core.update_work_node(
            &failed.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Failed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        core.create_work_node(NewWorkNode {
            kind: Some(WorkNodeKind::Milestone),
            ..task_node("Ship gate")
        })
        .await
        .unwrap();

        let plan = core
            .run_work_plan_with_options(
                &project.id,
                GateMode::Autonomous,
                Some(1),
                AgentKind::Codex,
                PermissionPolicy::ReadOnly,
                None,
                am_proto::WorkPlanOptions {
                    failure_mode: am_proto::PlanFailureMode::Continue,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ended = wait_for_plan_state(&core, &plan.id, WorkPlanRunState::Failed).await;
        assert_eq!(ended.completed_count, 1, "independent milestone completed");
        assert!(
            ended.error.as_deref().unwrap_or("").contains("skipped"),
            "error explains the skipped subtree: {:?}",
            ended.error
        );
    }

    #[tokio::test]
    async fn retry_budget_requeues_failed_nodes_then_exhausts() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Retry".into(),
                description: None,
            })
            .await
            .unwrap();
        let node = core
            .create_work_node(NewWorkNode {
                project_id: project.id.clone(),
                parent_id: None,
                kind: Some(WorkNodeKind::Task),
                title: "Unstable".into(),
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: Some(AgentKind::Codex),
                repo_ids: vec![],
                position_x: None,
                position_y: None,
                ..Default::default()
            })
            .await
            .unwrap();
        core.update_work_node(
            &node.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Failed),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let plan = am_db::repos::work_graph::create_plan_run(
            &core.db.pool,
            &project.id,
            GateMode::Autonomous,
            1,
            AgentKind::Codex,
            "read_only",
            None,
            None,
            1,
            &am_proto::WorkPlanOptions {
                failure_mode: am_proto::PlanFailureMode::Retry,
                max_node_retries: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // No attempts recorded yet: retry budget admits a requeue.
        let retried = core
            .retry_failed_plan_nodes(&plan, std::slice::from_ref(&node.id))
            .await
            .unwrap();
        assert!(retried);
        let requeued = core.get_work_node(&node.id).await.unwrap().unwrap();
        assert_eq!(requeued.status, TaskStatus::Queued);

        // Two attempts on record (initial + retry): budget exhausted.
        for _ in 0..2 {
            am_db::repos::work_graph::record_run(
                &core.db.pool,
                &node,
                AgentKind::Codex,
                &am_proto::new_id(),
            )
            .await
            .unwrap();
        }
        let runs = am_db::repos::work_graph::list_runs_for_node(&core.db.pool, &node.id)
            .await
            .unwrap();
        for run in &runs {
            am_db::repos::work_graph::attach_run_to_plan(&core.db.pool, &run.run_ref, &plan.id)
                .await
                .unwrap();
        }
        core.update_work_node(
            &node.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Failed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let retried = core
            .retry_failed_plan_nodes(&plan, std::slice::from_ref(&node.id))
            .await
            .unwrap();
        assert!(!retried, "retry budget must be exhausted after 2 attempts");
    }

    #[tokio::test]
    async fn blocker_completion_announces_resolution_to_dependents() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Unblock".into(),
                description: None,
            })
            .await
            .unwrap();
        let task_node = |title: &str| NewWorkNode {
            project_id: project.id.clone(),
            parent_id: None,
            kind: Some(WorkNodeKind::Task),
            title: title.into(),
            description: None,
            priority: TaskPriority::Medium,
            primary_agent: Some(AgentKind::Codex),
            repo_ids: vec![],
            position_x: None,
            position_y: None,
            ..Default::default()
        };
        let blocker = core.create_work_node(task_node("Schema")).await.unwrap();
        let dependent = core.create_work_node(task_node("API")).await.unwrap();
        core.connect_work_nodes(NewWorkEdge {
            project_id: project.id.clone(),
            source_id: dependent.id.clone(),
            target_id: blocker.id.clone(),
            kind: WorkEdgeKind::DependsOn,
            label: None,
        })
        .await
        .unwrap();

        core.update_work_node(
            &blocker.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let activity = core.list_activity(Some(&project.id), 100).await.unwrap();
        let resolved = activity
            .iter()
            .find(|event| event.kind == "work.blocker_resolved")
            .expect("blocker resolution announced");
        assert_eq!(
            resolved.payload.get("node_id").and_then(|v| v.as_str()),
            Some(dependent.id.as_str())
        );
        assert_eq!(
            resolved.payload.get("blocker_id").and_then(|v| v.as_str()),
            Some(blocker.id.as_str())
        );
    }

    #[tokio::test]
    async fn context_packet_includes_completed_prerequisite_handoffs() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Handoffs".into(),
                description: None,
            })
            .await
            .unwrap();

        let new_node = |title: &str| NewWorkNode {
            project_id: project.id.clone(),
            parent_id: None,
            kind: Some(WorkNodeKind::Task),
            title: title.into(),
            description: None,
            priority: TaskPriority::Medium,
            primary_agent: Some(AgentKind::Codex),
            repo_ids: vec![],
            position_x: None,
            position_y: None,
            ..Default::default()
        };
        let blocker = core
            .create_work_node(new_node("Design schema"))
            .await
            .unwrap();
        let dependent = core.create_work_node(new_node("Build API")).await.unwrap();
        core.connect_work_nodes(NewWorkEdge {
            project_id: project.id.clone(),
            source_id: dependent.id.clone(),
            target_id: blocker.id.clone(),
            kind: WorkEdgeKind::DependsOn,
            label: None,
        })
        .await
        .unwrap();

        // Prerequisite finishes and archives a handoff.
        core.update_work_node(
            &blocker.id,
            WorkNodeUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let blocker_task = blocker.task_id.clone().unwrap();
        am_db::repos::task_context::insert_handoff(
            &core.db.pool,
            &am_proto::TaskHandoff {
                id: am_proto::new_id(),
                task_id: blocker_task.clone(),
                session_id: "session-1".into(),
                agent: AgentKind::Codex,
                status: "completed".into(),
                summary: "Schema landed in migrations/0042_users.sql.".into(),
                changed_files: vec!["migrations/0042_users.sql".into()],
                next_actions: "Build endpoints on top.".into(),
                created_at: am_proto::now(),
            },
        )
        .await
        .unwrap();

        let packet = core.preview_context_packet(&dependent.id).await.unwrap();
        let handoff = packet
            .inclusions
            .iter()
            .find(|inclusion| inclusion.source_kind == "handoff")
            .expect("prerequisite handoff included");
        assert!(handoff.snippet.contains("0042_users.sql"));
        assert!(handoff.title.contains("Design schema"));
        assert!(packet
            .inclusions
            .iter()
            .all(|inclusion| inclusion.estimated_tokens > 0));

        // Archive round-trip.
        let archived = core.list_task_handoffs(&blocker_task, 10).await.unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].changed_files, vec!["migrations/0042_users.sql"]);
    }

    #[tokio::test]
    async fn knowledge_doc_crud() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Docs".into(),
                description: None,
            })
            .await
            .unwrap();

        let doc = core
            .create_knowledge_doc(NewKnowledgeDoc {
                project_id: project.id.clone(),
                title: "Conventions".into(),
                body: "Use arg arrays.".into(),
            })
            .await
            .unwrap();

        let updated = core
            .update_knowledge_doc(
                &doc.id,
                KnowledgeDocUpdate {
                    title: Some("Conventions v2".into()),
                    body: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.title, "Conventions v2");
        assert_eq!(updated.body, "Use arg arrays.");

        let docs = core.list_knowledge_docs(&project.id).await.unwrap();
        assert_eq!(docs.len(), 1);

        core.delete_knowledge_doc(&doc.id).await.unwrap();
        assert!(core
            .list_knowledge_docs(&project.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn memory_notes_scope_by_project_and_task() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Mem".into(),
                description: None,
            })
            .await
            .unwrap();
        let task = core
            .create_task(NewTask {
                project_id: project.id.clone(),
                title: "T".into(),
                repo_id: None,
                description: None,
                priority: TaskPriority::Medium,
                primary_agent: None,
                ..Default::default()
            })
            .await
            .unwrap();

        core.create_memory_note(NewMemoryNote {
            project_id: project.id.clone(),
            task_id: None,
            body: "project fact".into(),
        })
        .await
        .unwrap();
        core.create_memory_note(NewMemoryNote {
            project_id: project.id.clone(),
            task_id: Some(task.id.clone()),
            body: "task fact".into(),
        })
        .await
        .unwrap();

        // Project listing excludes task-scoped notes.
        let project_mem = core.list_project_memory(&project.id).await.unwrap();
        assert_eq!(project_mem.len(), 1);
        assert_eq!(project_mem[0].body, "project fact");

        let task_mem = core.list_task_memory(&task.id).await.unwrap();
        assert_eq!(task_mem.len(), 1);
        assert_eq!(task_mem[0].body, "task fact");

        core.delete_memory_note(&project_mem[0].id).await.unwrap();
        assert!(core
            .list_project_memory(&project.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn git_automation_defaults_off_and_persists() {
        use am_proto::GitAutomation;
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Git".into(),
                description: None,
            })
            .await
            .unwrap();

        // Off by default (opt-in).
        let initial = core.get_git_automation(&project.id).await.unwrap();
        assert!(!initial.auto_commit && !initial.auto_push);

        core.set_git_automation(
            &project.id,
            GitAutomation {
                auto_commit: true,
                auto_push: false,
            },
        )
        .await
        .unwrap();
        let saved = core.get_git_automation(&project.id).await.unwrap();
        assert!(saved.auto_commit && !saved.auto_push);
    }

    #[tokio::test]
    async fn agent_probe_preserves_unexpired_usage_limit() {
        use am_agents::AgentInstallStatus;
        use am_proto::{now, AgentKind, AvailabilityState};

        let core = core().await;
        let probe = |reset_known: bool| AgentInstallStatus {
            kind: AgentKind::ClaudeCode,
            installed: true,
            authenticated: true,
            version: reset_known.then(|| "1.0".to_string()),
            binary_path: None,
        };

        // An unexpired limit must survive a routine re-detect (otherwise a probe
        // would wrongly flip a limited agent back to available and the scheduler
        // would launch it straight into another limit).
        let future = now() + chrono::Duration::hours(1);
        core.mark_agent_limited(AgentKind::ClaudeCode, Some(future))
            .await
            .unwrap();
        let status = core.record_agent_probe(probe(true)).await.unwrap();
        assert_eq!(status.availability, AvailabilityState::Limited);
        assert!(status.reset_at.is_some());

        // Once cleared, the next probe reports available.
        core.mark_agent_available(AgentKind::ClaudeCode)
            .await
            .unwrap();
        let status = core.record_agent_probe(probe(true)).await.unwrap();
        assert_eq!(status.availability, AvailabilityState::Available);
        assert!(status.reset_at.is_none());

        // A no-reset limit is now bounded by the policy's retry window rather than
        // cleared on the next probe: `mark_agent_limited` synthesizes a reset time
        // so the scheduler waits a bounded interval instead of relaunching straight
        // back into the same limit. The limit therefore survives a healthy probe
        // until that synthesized window elapses.
        core.mark_agent_limited(AgentKind::ClaudeCode, None)
            .await
            .unwrap();
        let status = core.record_agent_probe(probe(false)).await.unwrap();
        assert_eq!(status.availability, AvailabilityState::Limited);
        assert!(status.reset_at.is_some());
    }

    #[tokio::test]
    async fn full_text_search_spans_entities_and_tracks_edits() {
        let core = core().await;
        let project = core
            .create_project(NewProject {
                name: "Search".into(),
                description: None,
            })
            .await
            .unwrap();

        let task = core
            .create_task(NewTask {
                project_id: project.id.clone(),
                title: "Implement authentication flow".into(),
                repo_id: None,
                description: Some("OAuth device login".into()),
                priority: TaskPriority::Medium,
                primary_agent: None,
                ..Default::default()
            })
            .await
            .unwrap();
        core.create_knowledge_doc(NewKnowledgeDoc {
            project_id: project.id.clone(),
            title: "Conventions".into(),
            body: "Always validate paths before spawning.".into(),
        })
        .await
        .unwrap();
        core.create_memory_note(NewMemoryNote {
            project_id: project.id.clone(),
            task_id: None,
            body: "Auth tokens live in the keychain.".into(),
        })
        .await
        .unwrap();

        // Prefix match across kinds.
        let hits = core.search("auth", Some(&project.id), 20).await.unwrap();
        let kinds: Vec<&str> = hits.iter().map(|h| h.kind.as_str()).collect();
        assert!(kinds.contains(&"task"), "task hit: {kinds:?}");
        assert!(kinds.contains(&"memory"), "memory hit: {kinds:?}");

        // Doc body is indexed.
        let doc_hits = core
            .search("validate", Some(&project.id), 20)
            .await
            .unwrap();
        assert!(doc_hits.iter().any(|h| h.kind == "doc"));

        // Edits propagate through the triggers: the old term disappears, the new
        // one is found.
        core.update_task(
            &task.id,
            TaskUpdate {
                title: Some("Implement billing flow".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(core
            .search("authentication", Some(&project.id), 20)
            .await
            .unwrap()
            .iter()
            .all(|h| h.kind != "task"));
        assert!(core
            .search("billing", Some(&project.id), 20)
            .await
            .unwrap()
            .iter()
            .any(|h| h.kind == "task"));

        // Empty / punctuation-only queries are safe and yield nothing.
        assert!(core
            .search("   ", Some(&project.id), 20)
            .await
            .unwrap()
            .is_empty());
        assert!(core
            .search("\"; --", Some(&project.id), 20)
            .await
            .unwrap()
            .is_empty());
    }
}
