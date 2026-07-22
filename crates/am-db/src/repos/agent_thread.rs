use am_proto::{
    new_id, now, AgentKind, AgentThread, AgentThreadUpdate, ComputeProviderKind, ExecutionBackend,
    LocalModelProviderKind, ModelTargetKind, NewAgentThread, NewWorkbenchSessionGroup, TaskBudget,
    TaskStatus, WorkbenchSessionGroup, WorkbenchSessionGroupUpdate,
};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct AgentThreadRow {
    id: String,
    project_id: Option<String>,
    group_id: Option<String>,
    title: String,
    status: String,
    active_agent: Option<String>,
    preferred_agent: Option<String>,
    permission: String,
    execution_backend: String,
    model: Option<String>,
    reasoning: Option<String>,
    local_provider: Option<String>,
    local_base_url: Option<String>,
    model_target: String,
    compute_lease_id: Option<String>,
    compute_provider: Option<String>,
    estimated_compute_cost_usd: Option<f64>,
    fallback_model_target: Option<String>,
    original_agent: Option<String>,
    fallback_agent: Option<String>,
    original_model: Option<String>,
    fallback_model: Option<String>,
    original_local_provider: Option<String>,
    fallback_local_provider: Option<String>,
    original_local_base_url: Option<String>,
    fallback_local_base_url: Option<String>,
    switch_back_pending: bool,
    limit_reset_at: Option<DateTime<Utc>>,
    switch_back: bool,
    handoff_state: String,
    objective: String,
    decisions: String,
    progress: String,
    open_questions: String,
    next_actions: String,
    task_budget: String,
    sort_order: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct WorkbenchSessionGroupRow {
    id: String,
    project_id: Option<String>,
    name: String,
    color: String,
    collapsed: bool,
    sort_order: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<WorkbenchSessionGroupRow> for WorkbenchSessionGroup {
    fn from(row: WorkbenchSessionGroupRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            name: row.name,
            color: row.color,
            collapsed: row.collapsed,
            sort_order: row.sort_order,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl TryFrom<AgentThreadRow> for AgentThread {
    type Error = DbError;

    fn try_from(r: AgentThreadRow) -> Result<Self, DbError> {
        let parse_agent = |value: Option<String>| -> Result<Option<AgentKind>, DbError> {
            value
                .map(|s| AgentKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
                .transpose()
        };
        let parse_provider =
            |value: Option<String>| -> Result<Option<LocalModelProviderKind>, DbError> {
                value
                    .map(|s| {
                        LocalModelProviderKind::parse(&s)
                            .ok_or_else(|| DbError::InvalidEnum(s.clone()))
                    })
                    .transpose()
            };
        Ok(AgentThread {
            id: r.id,
            project_id: r.project_id,
            group_id: r.group_id,
            title: r.title,
            status: TaskStatus::parse(&r.status)
                .ok_or_else(|| DbError::InvalidEnum(r.status.clone()))?,
            active_agent: parse_agent(r.active_agent)?,
            preferred_agent: parse_agent(r.preferred_agent)?,
            permission: r.permission,
            execution_backend: ExecutionBackend::parse(&r.execution_backend)
                .ok_or_else(|| DbError::InvalidEnum(r.execution_backend.clone()))?,
            model: r.model,
            reasoning: r.reasoning,
            local_provider: parse_provider(r.local_provider)?,
            local_base_url: r.local_base_url,
            model_target: ModelTargetKind::parse(&r.model_target)
                .ok_or_else(|| DbError::InvalidEnum(r.model_target.clone()))?,
            compute_lease_id: r.compute_lease_id,
            compute_provider: parse_compute_provider(r.compute_provider)?,
            estimated_compute_cost_usd: r.estimated_compute_cost_usd,
            fallback_model_target: parse_model_target(r.fallback_model_target)?,
            original_agent: parse_agent(r.original_agent)?,
            fallback_agent: parse_agent(r.fallback_agent)?,
            original_model: r.original_model,
            fallback_model: r.fallback_model,
            original_local_provider: parse_provider(r.original_local_provider)?,
            fallback_local_provider: parse_provider(r.fallback_local_provider)?,
            original_local_base_url: r.original_local_base_url,
            fallback_local_base_url: r.fallback_local_base_url,
            switch_back_pending: r.switch_back_pending,
            limit_reset_at: r.limit_reset_at,
            switch_back: r.switch_back,
            handoff_state: r.handoff_state,
            objective: r.objective,
            decisions: r.decisions,
            progress: r.progress,
            open_questions: r.open_questions,
            next_actions: r.next_actions,
            task_budget: parse_task_budget(r.task_budget)?,
            sort_order: r.sort_order,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

fn parse_compute_provider(value: Option<String>) -> Result<Option<ComputeProviderKind>, DbError> {
    value
        .map(|s| ComputeProviderKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
        .transpose()
}

fn parse_model_target(value: Option<String>) -> Result<Option<ModelTargetKind>, DbError> {
    value
        .map(|s| ModelTargetKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
        .transpose()
}

fn parse_task_budget(value: String) -> Result<TaskBudget, DbError> {
    let raw: serde_json::Value =
        serde_json::from_str(&value).map_err(|err| DbError::Serde(err.to_string()))?;
    // Older local databases may still contain the experimental Claude
    // percentage shape. Claude subscription windows are not a reliable host
    // contract, so safely migrate that legacy setting to no limit.
    if raw.get("mode").and_then(serde_json::Value::as_str) == Some("claude_percent") {
        return Ok(TaskBudget::Unlimited);
    }
    serde_json::from_value(raw).map_err(|err| DbError::Serde(err.to_string()))
}

const SELECT: &str = "SELECT id, project_id, group_id, title, status, active_agent, preferred_agent, \
    permission, execution_backend, model, reasoning, local_provider, local_base_url, \
    model_target, compute_lease_id, compute_provider, estimated_compute_cost_usd, fallback_model_target, \
    original_agent, fallback_agent, original_model, fallback_model, original_local_provider, \
    fallback_local_provider, original_local_base_url, fallback_local_base_url, \
    switch_back_pending, limit_reset_at, switch_back, handoff_state, objective, decisions, \
    progress, open_questions, next_actions, task_budget, sort_order, created_at, updated_at FROM agent_threads";

pub async fn create(pool: &SqlitePool, input: NewAgentThread) -> Result<AgentThread, DbError> {
    let ts = now();
    if let Some(task_budget) = input.task_budget.as_ref() {
        task_budget.validate().map_err(DbError::Serde)?;
    }
    let thread = AgentThread {
        id: new_id(),
        project_id: input.project_id,
        group_id: input.group_id,
        title: input.title,
        status: TaskStatus::Draft,
        active_agent: input.preferred_agent,
        preferred_agent: input.preferred_agent,
        permission: input
            .permission
            .unwrap_or_else(|| "workspace_write".to_string()),
        execution_backend: input.execution_backend.unwrap_or_default(),
        model: input.model,
        reasoning: input.reasoning,
        local_provider: input.local_provider,
        local_base_url: input.local_base_url,
        model_target: input.model_target.unwrap_or_else(|| {
            if input.local_provider.is_some() {
                ModelTargetKind::LocalProvider
            } else {
                ModelTargetKind::FrontierDefault
            }
        }),
        compute_lease_id: input.compute_lease_id,
        compute_provider: input.compute_provider,
        estimated_compute_cost_usd: input.estimated_compute_cost_usd,
        fallback_model_target: input.fallback_model_target,
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
        handoff_state: "none".to_string(),
        objective: input.objective.unwrap_or_default(),
        decisions: String::new(),
        progress: String::new(),
        open_questions: String::new(),
        next_actions: String::new(),
        task_budget: input.task_budget.unwrap_or_default(),
        sort_order: input.sort_order.unwrap_or(0),
        created_at: ts,
        updated_at: ts,
    };

    sqlx::query(
        "INSERT INTO agent_threads (id, project_id, group_id, title, status, active_agent, preferred_agent, \
         permission, execution_backend, model, reasoning, local_provider, local_base_url, \
         model_target, compute_lease_id, compute_provider, estimated_compute_cost_usd, fallback_model_target, \
         original_agent, fallback_agent, original_model, fallback_model, original_local_provider, \
         fallback_local_provider, original_local_base_url, fallback_local_base_url, \
         switch_back_pending, limit_reset_at, switch_back, handoff_state, objective, decisions, \
         progress, open_questions, next_actions, task_budget, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&thread.id)
    .bind(&thread.project_id)
    .bind(&thread.group_id)
    .bind(&thread.title)
    .bind(thread.status.as_str())
    .bind(thread.active_agent.map(|a| a.as_str()))
    .bind(thread.preferred_agent.map(|a| a.as_str()))
    .bind(&thread.permission)
    .bind(thread.execution_backend.as_str())
    .bind(&thread.model)
    .bind(&thread.reasoning)
    .bind(thread.local_provider.map(|provider| provider.as_str()))
    .bind(&thread.local_base_url)
    .bind(thread.model_target.as_str())
    .bind(&thread.compute_lease_id)
    .bind(thread.compute_provider.map(|provider| provider.as_str()))
    .bind(thread.estimated_compute_cost_usd)
    .bind(thread.fallback_model_target.map(|target| target.as_str()))
    .bind(thread.original_agent.map(|a| a.as_str()))
    .bind(thread.fallback_agent.map(|a| a.as_str()))
    .bind(&thread.original_model)
    .bind(&thread.fallback_model)
    .bind(thread.original_local_provider.map(|provider| provider.as_str()))
    .bind(thread.fallback_local_provider.map(|provider| provider.as_str()))
    .bind(&thread.original_local_base_url)
    .bind(&thread.fallback_local_base_url)
    .bind(thread.switch_back_pending)
    .bind(thread.limit_reset_at)
    .bind(thread.switch_back)
    .bind(&thread.handoff_state)
    .bind(&thread.objective)
    .bind(&thread.decisions)
    .bind(&thread.progress)
    .bind(&thread.open_questions)
    .bind(&thread.next_actions)
    .bind(serde_json::to_string(&thread.task_budget).map_err(|err| DbError::Serde(err.to_string()))?)
    .bind(thread.sort_order)
    .bind(thread.created_at)
    .bind(thread.updated_at)
    .execute(pool)
    .await?;

    Ok(thread)
}

pub async fn list(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<AgentThread>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, AgentThreadRow>(&format!(
                "{SELECT} WHERE project_id = ? ORDER BY group_id IS NULL, group_id, sort_order ASC, updated_at DESC"
            ))
            .bind(project_id)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, AgentThreadRow>(&format!(
                "{SELECT} ORDER BY group_id IS NULL, group_id, sort_order ASC, updated_at DESC"
            ))
                .fetch_all(pool)
                .await?
        }
    };
    rows.into_iter().map(AgentThread::try_from).collect()
}

pub async fn list_for_status(
    pool: &SqlitePool,
    status: TaskStatus,
    limit: i64,
) -> Result<Vec<AgentThread>, DbError> {
    let rows = sqlx::query_as::<_, AgentThreadRow>(&format!(
        "{SELECT} WHERE status = ? ORDER BY updated_at ASC LIMIT ?"
    ))
    .bind(status.as_str())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(AgentThread::try_from).collect()
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<AgentThread>, DbError> {
    let row = sqlx::query_as::<_, AgentThreadRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(AgentThread::try_from).transpose()
}

pub async fn update(
    pool: &SqlitePool,
    id: &str,
    patch: AgentThreadUpdate,
) -> Result<AgentThread, DbError> {
    let mut thread = get(pool, id).await?.ok_or(DbError::NotFound)?;
    if let Some(title) = patch.title {
        thread.title = title;
    }
    if let Some(status) = patch.status {
        thread.status = status;
    }
    if patch.active_agent.is_some() {
        thread.active_agent = patch.active_agent;
    }
    if let Some(group_id) = patch.group_id {
        thread.group_id = if group_id.is_empty() {
            None
        } else {
            Some(group_id)
        };
    }
    if patch.preferred_agent.is_some() {
        thread.preferred_agent = patch.preferred_agent;
    }
    if let Some(permission) = patch.permission {
        thread.permission = permission;
    }
    if let Some(execution_backend) = patch.execution_backend {
        thread.execution_backend = execution_backend;
    }
    if let Some(model) = patch.model {
        thread.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(reasoning) = patch.reasoning {
        thread.reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
    }
    if patch.local_provider.is_some() {
        thread.local_provider = patch.local_provider;
    }
    if let Some(local_base_url) = patch.local_base_url {
        thread.local_base_url = if local_base_url.is_empty() {
            None
        } else {
            Some(local_base_url)
        };
    }
    if let Some(model_target) = patch.model_target {
        thread.model_target = model_target;
    }
    if let Some(compute_lease_id) = patch.compute_lease_id {
        thread.compute_lease_id = if compute_lease_id.is_empty() {
            None
        } else {
            Some(compute_lease_id)
        };
    }
    if patch.compute_provider.is_some() {
        thread.compute_provider = patch.compute_provider;
    }
    if patch.estimated_compute_cost_usd.is_some() {
        thread.estimated_compute_cost_usd = patch.estimated_compute_cost_usd;
    }
    if patch.fallback_model_target.is_some() {
        thread.fallback_model_target = patch.fallback_model_target;
    }
    if let Some(objective) = patch.objective {
        thread.objective = objective;
    }
    if let Some(decisions) = patch.decisions {
        thread.decisions = decisions;
    }
    if let Some(progress) = patch.progress {
        thread.progress = progress;
    }
    if let Some(open_questions) = patch.open_questions {
        thread.open_questions = open_questions;
    }
    if let Some(next_actions) = patch.next_actions {
        thread.next_actions = next_actions;
    }
    if let Some(task_budget) = patch.task_budget {
        task_budget.validate().map_err(DbError::Serde)?;
        thread.task_budget = task_budget;
    }
    save(pool, &thread).await
}

pub async fn save(pool: &SqlitePool, thread: &AgentThread) -> Result<AgentThread, DbError> {
    let updated_at = now();
    sqlx::query(
        "UPDATE agent_threads SET title = ?, status = ?, active_agent = ?, preferred_agent = ?, \
         group_id = ?, \
         permission = ?, execution_backend = ?, model = ?, reasoning = ?, local_provider = ?, \
         local_base_url = ?, model_target = ?, compute_lease_id = ?, compute_provider = ?, \
         estimated_compute_cost_usd = ?, fallback_model_target = ?, \
         original_agent = ?, fallback_agent = ?, original_model = ?, \
         fallback_model = ?, original_local_provider = ?, fallback_local_provider = ?, \
         original_local_base_url = ?, fallback_local_base_url = ?, switch_back_pending = ?, \
         limit_reset_at = ?, switch_back = ?, handoff_state = ?, objective = ?, decisions = ?, \
         progress = ?, open_questions = ?, next_actions = ?, task_budget = ?, sort_order = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&thread.title)
    .bind(thread.status.as_str())
    .bind(thread.active_agent.map(|a| a.as_str()))
    .bind(thread.preferred_agent.map(|a| a.as_str()))
    .bind(&thread.group_id)
    .bind(&thread.permission)
    .bind(thread.execution_backend.as_str())
    .bind(&thread.model)
    .bind(&thread.reasoning)
    .bind(thread.local_provider.map(|provider| provider.as_str()))
    .bind(&thread.local_base_url)
    .bind(thread.model_target.as_str())
    .bind(&thread.compute_lease_id)
    .bind(thread.compute_provider.map(|provider| provider.as_str()))
    .bind(thread.estimated_compute_cost_usd)
    .bind(thread.fallback_model_target.map(|target| target.as_str()))
    .bind(thread.original_agent.map(|a| a.as_str()))
    .bind(thread.fallback_agent.map(|a| a.as_str()))
    .bind(&thread.original_model)
    .bind(&thread.fallback_model)
    .bind(
        thread
            .original_local_provider
            .map(|provider| provider.as_str()),
    )
    .bind(
        thread
            .fallback_local_provider
            .map(|provider| provider.as_str()),
    )
    .bind(&thread.original_local_base_url)
    .bind(&thread.fallback_local_base_url)
    .bind(thread.switch_back_pending)
    .bind(thread.limit_reset_at)
    .bind(thread.switch_back)
    .bind(&thread.handoff_state)
    .bind(&thread.objective)
    .bind(&thread.decisions)
    .bind(&thread.progress)
    .bind(&thread.open_questions)
    .bind(&thread.next_actions)
    .bind(serde_json::to_string(&thread.task_budget).map_err(|err| DbError::Serde(err.to_string()))?)
    .bind(thread.sort_order)
    .bind(updated_at)
    .bind(&thread.id)
    .execute(pool)
    .await?;

    get(pool, &thread.id).await?.ok_or(DbError::NotFound)
}

pub async fn assign_group(
    pool: &SqlitePool,
    thread_id: &str,
    group_id: Option<&str>,
) -> Result<AgentThread, DbError> {
    sqlx::query("UPDATE agent_threads SET group_id = ?, updated_at = ? WHERE id = ?")
        .bind(group_id)
        .bind(now())
        .bind(thread_id)
        .execute(pool)
        .await?;
    get(pool, thread_id).await?.ok_or(DbError::NotFound)
}

pub async fn create_group(
    pool: &SqlitePool,
    input: NewWorkbenchSessionGroup,
) -> Result<WorkbenchSessionGroup, DbError> {
    let ts = now();
    let group = WorkbenchSessionGroup {
        id: new_id(),
        project_id: input.project_id,
        name: input.name,
        color: input.color.unwrap_or_else(|| "teal".into()),
        collapsed: false,
        sort_order: input.sort_order.unwrap_or(0),
        created_at: ts,
        updated_at: ts,
    };
    sqlx::query(
        "INSERT INTO workbench_session_groups \
         (id, project_id, name, color, collapsed, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&group.id)
    .bind(&group.project_id)
    .bind(&group.name)
    .bind(&group.color)
    .bind(group.collapsed)
    .bind(group.sort_order)
    .bind(group.created_at)
    .bind(group.updated_at)
    .execute(pool)
    .await?;
    Ok(group)
}

pub async fn list_groups(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<WorkbenchSessionGroup>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, WorkbenchSessionGroupRow>(
                "SELECT id, project_id, name, color, collapsed, sort_order, created_at, updated_at \
                 FROM workbench_session_groups WHERE project_id = ? ORDER BY sort_order ASC, name ASC",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, WorkbenchSessionGroupRow>(
                "SELECT id, project_id, name, color, collapsed, sort_order, created_at, updated_at \
                 FROM workbench_session_groups ORDER BY sort_order ASC, name ASC",
            )
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(WorkbenchSessionGroup::from).collect())
}

pub async fn update_group(
    pool: &SqlitePool,
    id: &str,
    patch: WorkbenchSessionGroupUpdate,
) -> Result<WorkbenchSessionGroup, DbError> {
    let mut group = sqlx::query_as::<_, WorkbenchSessionGroupRow>(
        "SELECT id, project_id, name, color, collapsed, sort_order, created_at, updated_at \
         FROM workbench_session_groups WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(WorkbenchSessionGroup::from)
    .ok_or(DbError::NotFound)?;

    if let Some(name) = patch.name {
        group.name = name;
    }
    if let Some(color) = patch.color {
        group.color = color;
    }
    if let Some(collapsed) = patch.collapsed {
        group.collapsed = collapsed;
    }
    if let Some(sort_order) = patch.sort_order {
        group.sort_order = sort_order;
    }
    group.updated_at = now();

    sqlx::query(
        "UPDATE workbench_session_groups SET name = ?, color = ?, collapsed = ?, sort_order = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&group.name)
    .bind(&group.color)
    .bind(group.collapsed)
    .bind(group.sort_order)
    .bind(group.updated_at)
    .bind(&group.id)
    .execute(pool)
    .await?;

    Ok(group)
}

pub async fn delete_group(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE agent_threads SET group_id = NULL, updated_at = ? WHERE group_id = ?")
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM workbench_session_groups WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM agent_threads WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn pause_orphaned_running(pool: &SqlitePool) -> Result<u64, DbError> {
    let res =
        sqlx::query(
            "UPDATE agent_threads SET status = ?, handoff_state = ?, updated_at = ? WHERE status = 'running'",
        )
            .bind(TaskStatus::Queued.as_str())
            .bind("process_restarted")
            .bind(now())
            .execute(pool)
            .await?;
    Ok(res.rows_affected())
}
