use am_proto::{
    new_id, now, AgentKind, ContextPacket, EvaluationFollowUp, EvaluationVerdict, ExecutionBackend,
    GateMode, NewWorkEdge, NewWorkNode, PlanFailureMode, SessionState, TaskPriority, TaskStatus,
    WorkEdge, WorkEdgeKind, WorkEdgeUpdate, WorkGateEvaluation, WorkGraph, WorkNode, WorkNodeKind,
    WorkNodeRepoBinding, WorkNodeUpdate, WorkPlanOptions, WorkPlanRun, WorkPlanRunState, WorkRun,
};
use chrono::{DateTime, Utc};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::DbError;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RepoContextFile {
    pub id: String,
    pub repo_id: String,
    pub path: String,
    pub language: Option<String>,
    pub symbols_json: String,
    pub summary: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
    pub content_hash: String,
    pub indexed_at: DateTime<Utc>,
}

/// Just enough per-file metadata for the indexer's change detection, loaded in
/// one query so the walk can stat-compare without touching file contents.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RepoContextMeta {
    pub path: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
    pub content_hash: String,
}

/// A file row the indexer wants written (insert or update on `(repo_id, path)`).
#[derive(Debug, Clone)]
pub struct NewRepoContextFile {
    pub path: String,
    pub language: Option<&'static str>,
    pub symbols_json: String,
    pub summary: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
    pub content_hash: String,
}

/// Repo-level indexing state for the fast skip path.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RepoIndexState {
    pub repo_id: String,
    pub head_commit: Option<String>,
    pub dirty_digest: Option<String>,
    pub last_walk_at: DateTime<Utc>,
    pub file_count: i64,
}

#[derive(sqlx::FromRow)]
struct WorkNodeRow {
    id: String,
    project_id: String,
    parent_id: Option<String>,
    task_id: Option<String>,
    thread_id: Option<String>,
    kind: String,
    title: String,
    description: Option<String>,
    status: String,
    priority: String,
    primary_agent: Option<String>,
    position_x: f64,
    position_y: f64,
    width: Option<f64>,
    height: Option<f64>,
    position_locked: i64,
    sort_order: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<WorkNodeRow> for WorkNode {
    type Error = DbError;

    fn try_from(row: WorkNodeRow) -> Result<Self, Self::Error> {
        let primary_agent = row
            .primary_agent
            .map(|s| AgentKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
            .transpose()?;
        Ok(Self {
            id: row.id,
            project_id: row.project_id,
            parent_id: row.parent_id,
            task_id: row.task_id,
            thread_id: row.thread_id,
            kind: WorkNodeKind::parse(&row.kind)
                .ok_or_else(|| DbError::InvalidEnum(row.kind.clone()))?,
            title: row.title,
            description: row.description,
            status: TaskStatus::parse(&row.status)
                .ok_or_else(|| DbError::InvalidEnum(row.status.clone()))?,
            priority: TaskPriority::parse(&row.priority)
                .ok_or_else(|| DbError::InvalidEnum(row.priority.clone()))?,
            primary_agent,
            position_x: row.position_x,
            position_y: row.position_y,
            width: row.width,
            height: row.height,
            position_locked: row.position_locked != 0,
            sort_order: row.sort_order,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct WorkEdgeRow {
    id: String,
    project_id: String,
    source_id: String,
    target_id: String,
    kind: String,
    label: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<WorkEdgeRow> for WorkEdge {
    type Error = DbError;

    fn try_from(row: WorkEdgeRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            project_id: row.project_id,
            source_id: row.source_id,
            target_id: row.target_id,
            kind: WorkEdgeKind::parse(&row.kind)
                .ok_or_else(|| DbError::InvalidEnum(row.kind.clone()))?,
            label: row.label,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct RepoBindingRow {
    node_id: String,
    repo_id: String,
    repo_name: String,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    workspace_backend: String,
}

impl TryFrom<RepoBindingRow> for WorkNodeRepoBinding {
    type Error = DbError;

    fn try_from(row: RepoBindingRow) -> Result<Self, Self::Error> {
        Ok(Self {
            node_id: row.node_id,
            repo_id: row.repo_id,
            repo_name: row.repo_name,
            worktree_path: row.worktree_path,
            branch: row.branch,
            base_ref: row.base_ref,
            workspace_backend: ExecutionBackend::parse(&row.workspace_backend)
                .ok_or_else(|| DbError::InvalidEnum(row.workspace_backend.clone()))?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct WorkRunRow {
    id: String,
    node_id: String,
    task_id: Option<String>,
    thread_id: Option<String>,
    agent_kind: String,
    run_ref: String,
    state: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

impl TryFrom<WorkRunRow> for WorkRun {
    type Error = DbError;

    fn try_from(row: WorkRunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            node_id: row.node_id,
            task_id: row.task_id,
            thread_id: row.thread_id,
            agent_kind: AgentKind::parse(&row.agent_kind)
                .ok_or_else(|| DbError::InvalidEnum(row.agent_kind.clone()))?,
            run_ref: row.run_ref,
            state: parse_session_state(&row.state)?,
            started_at: row.started_at,
            ended_at: row.ended_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct WorkPlanRunRow {
    id: String,
    project_id: String,
    gate_mode: String,
    state: String,
    max_active_runs: i64,
    failure_mode: String,
    max_node_retries: i64,
    steer_dependents_on_unblock: i64,
    default_agent: Option<String>,
    default_permission: Option<String>,
    default_execution_backend: Option<String>,
    evaluator_policy_json: Option<String>,
    resume_after_node_id: Option<String>,
    policy_envelope_id: Option<String>,
    total_count: i64,
    completed_count: i64,
    active_count: i64,
    blocked_count: i64,
    error: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct WorkGateEvaluationRow {
    id: String,
    plan_run_id: Option<String>,
    node_id: String,
    evaluator_agent: Option<String>,
    verdict: String,
    confidence: f64,
    findings_json: String,
    required_followups_json: String,
    validation_commands_json: String,
    rationale: String,
    raw_output: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<WorkPlanRunRow> for WorkPlanRun {
    type Error = DbError;

    fn try_from(row: WorkPlanRunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            project_id: row.project_id,
            gate_mode: GateMode::parse(&row.gate_mode)
                .ok_or_else(|| DbError::InvalidEnum(row.gate_mode.clone()))?,
            state: WorkPlanRunState::parse(&row.state)
                .ok_or_else(|| DbError::InvalidEnum(row.state.clone()))?,
            max_active_runs: row.max_active_runs,
            failure_mode: PlanFailureMode::parse(&row.failure_mode)
                .ok_or_else(|| DbError::InvalidEnum(row.failure_mode.clone()))?,
            max_node_retries: row.max_node_retries,
            steer_dependents_on_unblock: row.steer_dependents_on_unblock != 0,
            default_agent: row
                .default_agent
                .map(|s| AgentKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
                .transpose()?,
            default_permission: row.default_permission,
            default_execution_backend: row
                .default_execution_backend
                .map(|s| ExecutionBackend::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
                .transpose()?,
            evaluator_policy_json: row.evaluator_policy_json,
            resume_after_node_id: row.resume_after_node_id,
            policy_envelope_id: row.policy_envelope_id,
            total_count: row.total_count,
            completed_count: row.completed_count,
            active_count: row.active_count,
            blocked_count: row.blocked_count,
            error: row.error,
            started_at: row.started_at,
            ended_at: row.ended_at,
            updated_at: row.updated_at,
        })
    }
}

impl TryFrom<WorkGateEvaluationRow> for WorkGateEvaluation {
    type Error = DbError;

    fn try_from(row: WorkGateEvaluationRow) -> Result<Self, Self::Error> {
        let findings = serde_json::from_str::<Vec<String>>(&row.findings_json).unwrap_or_default();
        let required_follow_ups =
            serde_json::from_str::<Vec<EvaluationFollowUp>>(&row.required_followups_json)
                .unwrap_or_default();
        let validation_commands =
            serde_json::from_str::<Vec<String>>(&row.validation_commands_json).unwrap_or_default();
        Ok(Self {
            id: row.id,
            plan_run_id: row.plan_run_id,
            node_id: row.node_id,
            evaluator_agent: row
                .evaluator_agent
                .map(|s| AgentKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
                .transpose()?,
            verdict: EvaluationVerdict::parse(&row.verdict)
                .ok_or_else(|| DbError::InvalidEnum(row.verdict.clone()))?,
            confidence: row.confidence,
            findings,
            required_follow_ups,
            validation_commands,
            rationale: row.rationale,
            raw_output: row.raw_output,
            created_at: row.created_at,
        })
    }
}

const NODE_SELECT: &str = "SELECT id, project_id, parent_id, task_id, thread_id, kind, title, \
    description, status, priority, primary_agent, position_x, position_y, width, height, \
    position_locked, sort_order, created_at, updated_at FROM work_nodes";

const EDGE_SELECT: &str = "SELECT id, project_id, source_id, target_id, kind, label, \
    created_at, updated_at FROM work_edges";

const PLAN_SELECT: &str = "SELECT id, project_id, gate_mode, state, max_active_runs, \
    failure_mode, max_node_retries, steer_dependents_on_unblock, \
    default_agent, default_permission, default_execution_backend, evaluator_policy_json, \
    resume_after_node_id, policy_envelope_id, total_count, completed_count, active_count, blocked_count, error, \
    started_at, ended_at, updated_at FROM work_plan_runs";

pub async fn graph(pool: &SqlitePool, project_id: &str) -> Result<WorkGraph, DbError> {
    let nodes = list_nodes(pool, project_id).await?;
    let edges = list_edges(pool, project_id).await?;
    let repo_bindings = list_repo_bindings_for_project(pool, project_id).await?;
    Ok(WorkGraph {
        project_id: project_id.to_string(),
        nodes,
        edges,
        repo_bindings,
    })
}

pub async fn list_nodes(pool: &SqlitePool, project_id: &str) -> Result<Vec<WorkNode>, DbError> {
    let rows = sqlx::query_as::<_, WorkNodeRow>(&format!(
        "{NODE_SELECT} WHERE project_id = ? ORDER BY sort_order ASC, updated_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkNode::try_from).collect()
}

pub async fn get_node(pool: &SqlitePool, id: &str) -> Result<Option<WorkNode>, DbError> {
    let row = sqlx::query_as::<_, WorkNodeRow>(&format!("{NODE_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(WorkNode::try_from).transpose()
}

pub async fn get_node_for_task(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<WorkNode>, DbError> {
    let row = sqlx::query_as::<_, WorkNodeRow>(&format!("{NODE_SELECT} WHERE task_id = ?"))
        .bind(task_id)
        .fetch_optional(pool)
        .await?;
    row.map(WorkNode::try_from).transpose()
}

pub async fn get_node_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Option<WorkNode>, DbError> {
    let row = sqlx::query_as::<_, WorkNodeRow>(&format!("{NODE_SELECT} WHERE thread_id = ?"))
        .bind(thread_id)
        .fetch_optional(pool)
        .await?;
    row.map(WorkNode::try_from).transpose()
}

pub async fn create_standalone_node(
    pool: &SqlitePool,
    input: NewWorkNode,
    kind: WorkNodeKind,
) -> Result<WorkNode, DbError> {
    let ts = now();
    let id = new_id();
    let sort_order: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sort_order) + 1, 0) FROM work_nodes WHERE project_id = ?",
    )
    .bind(&input.project_id)
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO work_nodes (id, project_id, parent_id, task_id, thread_id, kind, title, \
         description, status, priority, primary_agent, position_x, position_y, sort_order, \
         created_at, updated_at) VALUES (?, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.project_id)
    .bind(&input.parent_id)
    .bind(kind.as_str())
    .bind(input.title.trim())
    .bind(&input.description)
    .bind(TaskStatus::Draft.as_str())
    .bind(input.priority.as_str())
    .bind(input.primary_agent.map(|agent| agent.as_str()))
    .bind(input.position_x.unwrap_or(0.0))
    .bind(input.position_y.unwrap_or(0.0))
    .bind(sort_order)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    get_node(pool, &id).await?.ok_or(DbError::NotFound)
}

pub async fn update_node_fields(
    pool: &SqlitePool,
    id: &str,
    patch: WorkNodeUpdate,
) -> Result<WorkNode, DbError> {
    let mut node = get_node(pool, id).await?.ok_or(DbError::NotFound)?;
    if patch.parent_id.is_some() {
        node.parent_id = patch.parent_id;
    }
    if let Some(title) = patch.title {
        node.title = title;
    }
    if patch.description.is_some() {
        node.description = patch.description;
    }
    if let Some(status) = patch.status {
        node.status = status;
    }
    if let Some(priority) = patch.priority {
        node.priority = priority;
    }
    if patch.primary_agent.is_some() {
        node.primary_agent = patch.primary_agent;
    }
    if let Some(position_x) = patch.position_x {
        node.position_x = position_x;
    }
    if let Some(position_y) = patch.position_y {
        node.position_y = position_y;
    }
    if let Some(sort_order) = patch.sort_order {
        node.sort_order = sort_order;
    }
    node.updated_at = now();
    sqlx::query(
        "UPDATE work_nodes SET parent_id = ?, title = ?, description = ?, status = ?, \
         priority = ?, primary_agent = ?, position_x = ?, position_y = ?, sort_order = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&node.parent_id)
    .bind(&node.title)
    .bind(&node.description)
    .bind(node.status.as_str())
    .bind(node.priority.as_str())
    .bind(node.primary_agent.map(|agent| agent.as_str()))
    .bind(node.position_x)
    .bind(node.position_y)
    .bind(node.sort_order)
    .bind(node.updated_at)
    .bind(&node.id)
    .execute(pool)
    .await?;
    get_node(pool, id).await?.ok_or(DbError::NotFound)
}

/// Manual placement (drag / explicit move): pins the node so PreserveManual
/// layouts anchor it.
pub async fn move_node(
    pool: &SqlitePool,
    id: &str,
    parent_id: Option<String>,
    position_x: f64,
    position_y: f64,
) -> Result<WorkNode, DbError> {
    let updated_at = now();
    sqlx::query(
        "UPDATE work_nodes SET parent_id = ?, position_x = ?, position_y = ?, \
         position_locked = 1, updated_at = ? WHERE id = ?",
    )
    .bind(&parent_id)
    .bind(position_x)
    .bind(position_y)
    .bind(updated_at)
    .bind(id)
    .execute(pool)
    .await?;
    get_node(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn delete_node(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM work_nodes WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// One computed node placement from the layout engine.
#[derive(Debug, Clone)]
pub struct NodePlacement {
    pub node_id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Write a whole layout in one transaction (positions + sizes), optionally
/// clearing manual pins first (Force mode). Layout writes never set the pin.
pub async fn apply_layout(
    pool: &SqlitePool,
    project_id: &str,
    placements: &[NodePlacement],
    clear_locks: bool,
) -> Result<(), DbError> {
    let ts = now();
    let mut tx = pool.begin().await?;
    if clear_locks {
        sqlx::query("UPDATE work_nodes SET position_locked = 0 WHERE project_id = ?")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
    }
    for placement in placements {
        sqlx::query(
            "UPDATE work_nodes SET position_x = ?, position_y = ?, width = ?, height = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(placement.x)
        .bind(placement.y)
        .bind(placement.width)
        .bind(placement.height)
        .bind(ts)
        .bind(&placement.node_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list_edges(pool: &SqlitePool, project_id: &str) -> Result<Vec<WorkEdge>, DbError> {
    let rows = sqlx::query_as::<_, WorkEdgeRow>(&format!(
        "{EDGE_SELECT} WHERE project_id = ? ORDER BY created_at ASC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkEdge::try_from).collect()
}

pub async fn create_edge(pool: &SqlitePool, input: NewWorkEdge) -> Result<WorkEdge, DbError> {
    let ts = now();
    let id = new_id();
    sqlx::query(
        "INSERT INTO work_edges (id, project_id, source_id, target_id, kind, label, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(source_id, target_id, kind) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at",
    )
    .bind(&id)
    .bind(&input.project_id)
    .bind(&input.source_id)
    .bind(&input.target_id)
    .bind(input.kind.as_str())
    .bind(&input.label)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    let row = sqlx::query_as::<_, WorkEdgeRow>(&format!(
        "{EDGE_SELECT} WHERE source_id = ? AND target_id = ? AND kind = ?"
    ))
    .bind(&input.source_id)
    .bind(&input.target_id)
    .bind(input.kind.as_str())
    .fetch_one(pool)
    .await?;
    WorkEdge::try_from(row)
}

pub async fn get_edge(pool: &SqlitePool, id: &str) -> Result<Option<WorkEdge>, DbError> {
    let row = sqlx::query_as::<_, WorkEdgeRow>(&format!("{EDGE_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(WorkEdge::try_from).transpose()
}

pub async fn update_edge(
    pool: &SqlitePool,
    id: &str,
    patch: WorkEdgeUpdate,
) -> Result<WorkEdge, DbError> {
    let mut edge = get_edge(pool, id).await?.ok_or(DbError::NotFound)?;
    if let Some(source_id) = patch.source_id {
        edge.source_id = source_id;
    }
    if let Some(target_id) = patch.target_id {
        edge.target_id = target_id;
    }
    if let Some(kind) = patch.kind {
        edge.kind = kind;
    }
    if patch.label.is_some() {
        edge.label = patch.label.and_then(|label| {
            let trimmed = label.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
    }
    edge.updated_at = now();
    sqlx::query(
        "UPDATE work_edges SET source_id = ?, target_id = ?, kind = ?, label = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&edge.source_id)
    .bind(&edge.target_id)
    .bind(edge.kind.as_str())
    .bind(&edge.label)
    .bind(edge.updated_at)
    .bind(&edge.id)
    .execute(pool)
    .await?;
    get_edge(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn delete_edge(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM work_edges WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn blocking_edges_for_node(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Vec<WorkEdge>, DbError> {
    let rows = sqlx::query_as::<_, WorkEdgeRow>(&format!(
        "SELECT e.id, e.project_id, e.source_id, e.target_id, e.kind, e.label, \
         e.created_at, e.updated_at FROM work_edges e \
         JOIN work_nodes blocker ON \
           ((e.kind = 'blocks' AND blocker.id = e.source_id AND e.target_id = ?) OR \
            (e.kind = 'depends_on' AND blocker.id = e.target_id AND e.source_id = ?)) \
         WHERE blocker.status NOT IN ('done', 'cancelled')"
    ))
    .bind(node_id)
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkEdge::try_from).collect()
}

/// Prerequisite nodes of `node_id` (via gating edges) that finished
/// successfully — their handoffs are prime context for the dependent work.
pub async fn completed_gating_predecessors(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Vec<WorkNode>, DbError> {
    let rows = sqlx::query_as::<_, WorkNodeRow>(
        "SELECT n.id, n.project_id, n.parent_id, n.task_id, n.thread_id, n.kind, n.title, \
         n.description, n.status, n.priority, n.primary_agent, n.position_x, n.position_y, \
         n.width, n.height, n.position_locked, n.sort_order, n.created_at, n.updated_at \
         FROM work_edges e \
         JOIN work_nodes n ON \
           ((e.kind IN ('blocks', 'handoff') AND n.id = e.source_id AND e.target_id = ?) OR \
            (e.kind = 'depends_on' AND n.id = e.target_id AND e.source_id = ?)) \
         WHERE n.status = 'done'",
    )
    .bind(node_id)
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkNode::try_from).collect()
}

pub async fn replace_node_repos(
    pool: &SqlitePool,
    node_id: &str,
    repo_ids: &[String],
) -> Result<Vec<WorkNodeRepoBinding>, DbError> {
    sqlx::query("DELETE FROM work_node_repos WHERE node_id = ?")
        .bind(node_id)
        .execute(pool)
        .await?;
    for repo_id in repo_ids {
        sqlx::query(
            "INSERT INTO work_node_repos (node_id, repo_id, worktree_path, branch, base_ref, workspace_backend) \
             VALUES (?, ?, NULL, NULL, NULL, 'host')",
        )
        .bind(node_id)
        .bind(repo_id)
        .execute(pool)
        .await?;
    }
    list_repo_bindings(pool, node_id).await
}

pub async fn list_repo_bindings(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Vec<WorkNodeRepoBinding>, DbError> {
    let rows = sqlx::query_as::<_, RepoBindingRow>(
        "SELECT wnr.node_id, wnr.repo_id, r.name AS repo_name, wnr.worktree_path, wnr.branch, \
         wnr.base_ref, wnr.workspace_backend FROM work_node_repos wnr \
         JOIN repos r ON r.id = wnr.repo_id WHERE wnr.node_id = ? ORDER BY r.name ASC",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(WorkNodeRepoBinding::try_from)
        .collect()
}

pub async fn list_repo_bindings_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<WorkNodeRepoBinding>, DbError> {
    let rows = sqlx::query_as::<_, RepoBindingRow>(
        "SELECT wnr.node_id, wnr.repo_id, r.name AS repo_name, wnr.worktree_path, wnr.branch, \
         wnr.base_ref, wnr.workspace_backend FROM work_node_repos wnr \
         JOIN work_nodes wn ON wn.id = wnr.node_id \
         JOIN repos r ON r.id = wnr.repo_id WHERE wn.project_id = ? ORDER BY r.name ASC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(WorkNodeRepoBinding::try_from)
        .collect()
}

pub async fn conflicting_locks(pool: &SqlitePool, node_id: &str) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT wl.node_id FROM work_locks wl \
         JOIN work_node_repos wnr ON wnr.repo_id = wl.repo_id \
         WHERE wnr.node_id = ? AND wl.node_id != ?",
    )
    .bind(node_id)
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn acquire_locks(pool: &SqlitePool, node_id: &str) -> Result<(), DbError> {
    let ts = now();
    let bindings = list_repo_bindings(pool, node_id).await?;
    for binding in bindings {
        sqlx::query(
            "INSERT INTO work_locks (node_id, repo_id, path_glob, mode, acquired_at) \
             VALUES (?, ?, '*', 'write', ?)",
        )
        .bind(node_id)
        .bind(&binding.repo_id)
        .bind(ts)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn release_locks(pool: &SqlitePool, node_id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM work_locks WHERE node_id = ?")
        .bind(node_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn release_locks_for_task(pool: &SqlitePool, task_id: &str) -> Result<(), DbError> {
    if let Some(node) = get_node_for_task(pool, task_id).await? {
        release_locks(pool, &node.id).await?;
    }
    Ok(())
}

pub async fn release_locks_for_thread(pool: &SqlitePool, thread_id: &str) -> Result<(), DbError> {
    if let Some(node) = get_node_for_thread(pool, thread_id).await? {
        release_locks(pool, &node.id).await?;
    }
    Ok(())
}

pub async fn record_run(
    pool: &SqlitePool,
    node: &WorkNode,
    agent: AgentKind,
    run_ref: &str,
) -> Result<WorkRun, DbError> {
    let ts = now();
    let id = new_id();
    sqlx::query(
        "INSERT INTO work_runs (id, node_id, task_id, thread_id, agent_kind, run_ref, state, started_at, ended_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'running', ?, NULL)",
    )
    .bind(&id)
    .bind(&node.id)
    .bind(&node.task_id)
    .bind(&node.thread_id)
    .bind(agent.as_str())
    .bind(run_ref)
    .bind(ts)
    .execute(pool)
    .await?;
    get_run(pool, &id).await?.ok_or(DbError::NotFound)
}

pub async fn attach_run_to_plan(
    pool: &SqlitePool,
    run_ref: &str,
    plan_run_id: &str,
) -> Result<(), DbError> {
    sqlx::query("UPDATE work_runs SET plan_run_id = ? WHERE run_ref = ?")
        .bind(plan_run_id)
        .bind(run_ref)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn finish_runs_for_ref(
    pool: &SqlitePool,
    run_ref: &str,
    state: SessionState,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE work_runs SET state = ?, ended_at = ? WHERE run_ref = ? AND ended_at IS NULL",
    )
    .bind(state.as_str())
    .bind(now())
    .bind(run_ref)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_run(pool: &SqlitePool, id: &str) -> Result<Option<WorkRun>, DbError> {
    let row = sqlx::query_as::<_, WorkRunRow>(
        "SELECT id, node_id, task_id, thread_id, agent_kind, run_ref, state, started_at, ended_at \
         FROM work_runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(WorkRun::try_from).transpose()
}

pub async fn list_runs_for_node(pool: &SqlitePool, node_id: &str) -> Result<Vec<WorkRun>, DbError> {
    let rows = sqlx::query_as::<_, WorkRunRow>(
        "SELECT id, node_id, task_id, thread_id, agent_kind, run_ref, state, started_at, ended_at \
         FROM work_runs WHERE node_id = ? ORDER BY started_at ASC",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkRun::try_from).collect()
}

pub async fn list_running_runs_for_plan(
    pool: &SqlitePool,
    plan_run_id: &str,
) -> Result<Vec<WorkRun>, DbError> {
    let rows = sqlx::query_as::<_, WorkRunRow>(
        "SELECT id, node_id, task_id, thread_id, agent_kind, run_ref, state, started_at, ended_at \
         FROM work_runs WHERE plan_run_id = ? AND state = 'running' ORDER BY started_at ASC",
    )
    .bind(plan_run_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkRun::try_from).collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn create_plan_run(
    pool: &SqlitePool,
    project_id: &str,
    gate_mode: GateMode,
    max_active_runs: i64,
    default_agent: AgentKind,
    default_permission: &str,
    default_execution_backend: Option<ExecutionBackend>,
    evaluator_policy_json: Option<&str>,
    total_count: i64,
    options: &WorkPlanOptions,
) -> Result<WorkPlanRun, DbError> {
    let ts = now();
    let id = new_id();
    sqlx::query(
        "INSERT INTO work_plan_runs (id, project_id, gate_mode, state, max_active_runs, \
         failure_mode, max_node_retries, steer_dependents_on_unblock, \
         default_agent, default_permission, default_execution_backend, evaluator_policy_json, \
         resume_after_node_id, total_count, completed_count, active_count, blocked_count, error, \
         started_at, ended_at, updated_at) \
         VALUES (?, ?, ?, 'running', ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, 0, 0, 0, NULL, ?, NULL, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(gate_mode.as_str())
    .bind(max_active_runs)
    .bind(options.failure_mode.as_str())
    .bind(options.max_node_retries.max(0))
    .bind(options.steer_dependents_on_unblock as i64)
    .bind(default_agent.as_str())
    .bind(default_permission)
    .bind(default_execution_backend.map(|backend| backend.as_str()))
    .bind(evaluator_policy_json)
    .bind(total_count)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    get_plan_run(pool, &id).await?.ok_or(DbError::NotFound)
}

/// How many runs of `node_id` are attached to this plan (first attempt plus
/// retries) — the retry budget check.
pub async fn count_runs_for_node_in_plan(
    pool: &SqlitePool,
    plan_run_id: &str,
    node_id: &str,
) -> Result<i64, DbError> {
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM work_runs WHERE plan_run_id = ? AND node_id = ?")
            .bind(plan_run_id)
            .bind(node_id)
            .fetch_one(pool)
            .await?;
    Ok(count.0)
}

/// Nodes gated on `node_id` (its dependents via gating edges).
pub async fn gating_dependents(pool: &SqlitePool, node_id: &str) -> Result<Vec<WorkNode>, DbError> {
    let rows = sqlx::query_as::<_, WorkNodeRow>(
        "SELECT n.id, n.project_id, n.parent_id, n.task_id, n.thread_id, n.kind, n.title, \
         n.description, n.status, n.priority, n.primary_agent, n.position_x, n.position_y, \
         n.width, n.height, n.position_locked, n.sort_order, n.created_at, n.updated_at \
         FROM work_edges e \
         JOIN work_nodes n ON \
           ((e.kind IN ('blocks', 'handoff') AND e.source_id = ? AND n.id = e.target_id) OR \
            (e.kind = 'depends_on' AND e.target_id = ? AND n.id = e.source_id))",
    )
    .bind(node_id)
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkNode::try_from).collect()
}

pub async fn set_plan_run_resume_after_node(
    pool: &SqlitePool,
    id: &str,
    node_id: Option<&str>,
) -> Result<WorkPlanRun, DbError> {
    let ts = now();
    sqlx::query("UPDATE work_plan_runs SET resume_after_node_id = ?, updated_at = ? WHERE id = ?")
        .bind(node_id)
        .bind(ts)
        .bind(id)
        .execute(pool)
        .await?;
    get_plan_run(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn resume_plan_run(pool: &SqlitePool, id: &str) -> Result<WorkPlanRun, DbError> {
    let ts = now();
    sqlx::query(
        "UPDATE work_plan_runs SET state = 'running', error = NULL, resume_after_node_id = NULL, \
         ended_at = NULL, updated_at = ? WHERE id = ? AND state = 'paused'",
    )
    .bind(ts)
    .bind(id)
    .execute(pool)
    .await?;
    get_plan_run(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn get_plan_run(pool: &SqlitePool, id: &str) -> Result<Option<WorkPlanRun>, DbError> {
    let row = sqlx::query_as::<_, WorkPlanRunRow>(&format!("{PLAN_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(WorkPlanRun::try_from).transpose()
}

/// Paused plan runs across every project, oldest first. Lets the scheduler
/// check resumability with one query instead of scanning all projects' runs.
pub async fn list_paused_plan_runs(pool: &SqlitePool) -> Result<Vec<WorkPlanRun>, DbError> {
    let rows = sqlx::query_as::<_, WorkPlanRunRow>(&format!(
        "{PLAN_SELECT} WHERE state = 'paused' ORDER BY started_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkPlanRun::try_from).collect()
}

pub async fn list_plan_runs(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<WorkPlanRun>, DbError> {
    let rows = sqlx::query_as::<_, WorkPlanRunRow>(&format!(
        "{PLAN_SELECT} WHERE project_id = ? ORDER BY started_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkPlanRun::try_from).collect()
}

pub async fn update_plan_run_progress(
    pool: &SqlitePool,
    id: &str,
    state: WorkPlanRunState,
    completed_count: i64,
    active_count: i64,
    blocked_count: i64,
    error: Option<&str>,
    ended: bool,
) -> Result<WorkPlanRun, DbError> {
    let ts = now();
    let ended_at = ended.then_some(ts);
    sqlx::query(
        "UPDATE work_plan_runs SET state = ?, completed_count = ?, active_count = ?, \
         blocked_count = ?, error = ?, ended_at = COALESCE(?, ended_at), updated_at = ? WHERE id = ?",
    )
    .bind(state.as_str())
    .bind(completed_count)
    .bind(active_count)
    .bind(blocked_count)
    .bind(error)
    .bind(ended_at)
    .bind(ts)
    .bind(id)
    .execute(pool)
    .await?;
    get_plan_run(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn cancel_plan_run(pool: &SqlitePool, id: &str) -> Result<WorkPlanRun, DbError> {
    let ts = now();
    sqlx::query(
        "UPDATE work_plan_runs SET state = 'cancelled', ended_at = COALESCE(ended_at, ?), \
         updated_at = ? WHERE id = ?",
    )
    .bind(ts)
    .bind(ts)
    .bind(id)
    .execute(pool)
    .await?;
    get_plan_run(pool, id).await?.ok_or(DbError::NotFound)
}

pub async fn insert_gate_evaluation(
    pool: &SqlitePool,
    evaluation: &WorkGateEvaluation,
) -> Result<WorkGateEvaluation, DbError> {
    let findings_json = serde_json::to_string(&evaluation.findings).unwrap_or_else(|_| "[]".into());
    let followups_json =
        serde_json::to_string(&evaluation.required_follow_ups).unwrap_or_else(|_| "[]".into());
    let commands_json =
        serde_json::to_string(&evaluation.validation_commands).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO work_gate_evaluations (id, plan_run_id, node_id, evaluator_agent, verdict, \
         confidence, findings_json, required_followups_json, validation_commands_json, rationale, \
         raw_output, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&evaluation.id)
    .bind(&evaluation.plan_run_id)
    .bind(&evaluation.node_id)
    .bind(evaluation.evaluator_agent.map(|agent| agent.as_str()))
    .bind(evaluation.verdict.as_str())
    .bind(evaluation.confidence)
    .bind(&findings_json)
    .bind(&followups_json)
    .bind(&commands_json)
    .bind(&evaluation.rationale)
    .bind(&evaluation.raw_output)
    .bind(evaluation.created_at)
    .execute(pool)
    .await?;
    get_gate_evaluation(pool, &evaluation.id)
        .await?
        .ok_or(DbError::NotFound)
}

pub async fn get_gate_evaluation(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<WorkGateEvaluation>, DbError> {
    let row = sqlx::query_as::<_, WorkGateEvaluationRow>(
        "SELECT id, plan_run_id, node_id, evaluator_agent, verdict, confidence, findings_json, \
         required_followups_json, validation_commands_json, rationale, raw_output, created_at \
         FROM work_gate_evaluations WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(WorkGateEvaluation::try_from).transpose()
}

pub async fn list_gate_evaluations(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Vec<WorkGateEvaluation>, DbError> {
    let rows = sqlx::query_as::<_, WorkGateEvaluationRow>(
        "SELECT id, plan_run_id, node_id, evaluator_agent, verdict, confidence, findings_json, \
         required_followups_json, validation_commands_json, rationale, raw_output, created_at \
         FROM work_gate_evaluations WHERE node_id = ? ORDER BY created_at DESC",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(WorkGateEvaluation::try_from).collect()
}

pub async fn record_context_packet(
    pool: &SqlitePool,
    packet: &ContextPacket,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO context_packets (id, node_id, budget_bytes, used_bytes, summary, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&packet.id)
    .bind(&packet.node_id)
    .bind(packet.budget_bytes)
    .bind(packet.used_bytes)
    .bind(&packet.summary)
    .bind(packet.created_at)
    .execute(pool)
    .await?;

    for inclusion in &packet.inclusions {
        sqlx::query(
            "INSERT INTO context_inclusions (packet_id, source_kind, entity_id, title, snippet, reason, score, bytes, estimated_tokens) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&packet.id)
        .bind(&inclusion.source_kind)
        .bind(&inclusion.entity_id)
        .bind(&inclusion.title)
        .bind(&inclusion.snippet)
        .bind(&inclusion.reason)
        .bind(inclusion.score)
        .bind(inclusion.bytes)
        .bind(inclusion.estimated_tokens)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Apply one walk's worth of index changes atomically: upsert changed files,
/// drop vanished ones, and record the repo's walk state — a single write
/// transaction instead of a statement per file.
pub async fn apply_repo_context_changes(
    pool: &SqlitePool,
    repo_id: &str,
    upserts: &[NewRepoContextFile],
    deleted_paths: &[String],
    head_commit: Option<&str>,
    dirty_digest: Option<&str>,
    file_count: i64,
) -> Result<(), DbError> {
    let ts = now();
    let mut tx = pool.begin().await?;
    for file in upserts {
        sqlx::query(
            "INSERT INTO repo_context_index \
             (id, repo_id, path, language, symbols_json, summary, size_bytes, mtime_ms, content_hash, indexed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(repo_id, path) DO UPDATE SET language = excluded.language, \
              symbols_json = excluded.symbols_json, summary = excluded.summary, \
              size_bytes = excluded.size_bytes, mtime_ms = excluded.mtime_ms, \
              content_hash = excluded.content_hash, indexed_at = excluded.indexed_at",
        )
        .bind(new_id())
        .bind(repo_id)
        .bind(&file.path)
        .bind(file.language)
        .bind(&file.symbols_json)
        .bind(&file.summary)
        .bind(file.size_bytes)
        .bind(file.mtime_ms)
        .bind(&file.content_hash)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
    }
    for path in deleted_paths {
        sqlx::query("DELETE FROM repo_context_index WHERE repo_id = ? AND path = ?")
            .bind(repo_id)
            .bind(path)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO repo_index_state (repo_id, head_commit, dirty_digest, last_walk_at, file_count) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(repo_id) DO UPDATE SET head_commit = excluded.head_commit, \
          dirty_digest = excluded.dirty_digest, last_walk_at = excluded.last_walk_at, \
          file_count = excluded.file_count",
    )
    .bind(repo_id)
    .bind(head_commit)
    .bind(dirty_digest)
    .bind(ts)
    .bind(file_count)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn get_repo_index_state(
    pool: &SqlitePool,
    repo_id: &str,
) -> Result<Option<RepoIndexState>, DbError> {
    Ok(sqlx::query_as::<_, RepoIndexState>(
        "SELECT repo_id, head_commit, dirty_digest, last_walk_at, file_count \
         FROM repo_index_state WHERE repo_id = ?",
    )
    .bind(repo_id)
    .fetch_optional(pool)
    .await?)
}

/// Existing index metadata for one repo, for stat-based change detection.
pub async fn list_repo_context_meta(
    pool: &SqlitePool,
    repo_id: &str,
) -> Result<Vec<RepoContextMeta>, DbError> {
    Ok(sqlx::query_as::<_, RepoContextMeta>(
        "SELECT path, size_bytes, mtime_ms, content_hash FROM repo_context_index WHERE repo_id = ?",
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_repo_context_files(
    pool: &SqlitePool,
    repo_ids: &[String],
    limit: i64,
) -> Result<Vec<RepoContextFile>, DbError> {
    if repo_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
        "SELECT id, repo_id, path, language, symbols_json, summary, size_bytes, mtime_ms, content_hash, indexed_at \
         FROM repo_context_index WHERE repo_id IN (",
    );
    let mut separated = qb.separated(", ");
    for repo_id in repo_ids {
        separated.push_bind(repo_id);
    }
    separated.push_unseparated(") ORDER BY indexed_at DESC LIMIT ");
    qb.push_bind(limit);
    Ok(qb.build_query_as().fetch_all(pool).await?)
}

fn parse_session_state(value: &str) -> Result<SessionState, DbError> {
    Ok(match value {
        "running" => SessionState::Running,
        "completed" => SessionState::Completed,
        "interrupted" => SessionState::Interrupted,
        "failed" => SessionState::Failed,
        other => return Err(DbError::InvalidEnum(other.to_string())),
    })
}
