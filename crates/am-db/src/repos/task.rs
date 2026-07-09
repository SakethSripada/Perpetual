use am_proto::{
    new_id, now, AgentKind, ComputeProviderKind, ModelTargetKind, NewTask, Task, TaskPriority,
    TaskStatus, TaskUpdate,
};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: String,
    project_id: String,
    title: String,
    description: Option<String>,
    status: String,
    priority: String,
    primary_agent: Option<String>,
    model: Option<String>,
    model_target: String,
    compute_lease_id: Option<String>,
    compute_provider: Option<String>,
    estimated_compute_cost_usd: Option<f64>,
    fallback_model_target: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<TaskRow> for Task {
    type Error = DbError;
    fn try_from(r: TaskRow) -> Result<Self, DbError> {
        let status =
            TaskStatus::parse(&r.status).ok_or_else(|| DbError::InvalidEnum(r.status.clone()))?;
        let priority = TaskPriority::parse(&r.priority)
            .ok_or_else(|| DbError::InvalidEnum(r.priority.clone()))?;
        let primary_agent = match r.primary_agent {
            Some(ref s) => {
                Some(AgentKind::parse(s).ok_or_else(|| DbError::InvalidEnum(s.clone()))?)
            }
            None => None,
        };
        Ok(Task {
            id: r.id,
            project_id: r.project_id,
            title: r.title,
            description: r.description,
            status,
            priority,
            primary_agent,
            model: r.model,
            model_target: ModelTargetKind::parse(&r.model_target)
                .ok_or_else(|| DbError::InvalidEnum(r.model_target.clone()))?,
            compute_lease_id: r.compute_lease_id,
            compute_provider: parse_compute_provider(r.compute_provider)?,
            estimated_compute_cost_usd: r.estimated_compute_cost_usd,
            fallback_model_target: parse_model_target(r.fallback_model_target)?,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

const SELECT: &str = "SELECT id, project_id, title, description, status, priority, \
    primary_agent, model, model_target, compute_lease_id, compute_provider, \
    estimated_compute_cost_usd, fallback_model_target, created_at, updated_at FROM tasks";

pub async fn create(pool: &SqlitePool, input: NewTask) -> Result<Task, DbError> {
    let ts = now();
    let task = Task {
        id: new_id(),
        project_id: input.project_id,
        title: input.title,
        description: input.description,
        status: TaskStatus::Draft,
        priority: input.priority,
        primary_agent: input.primary_agent,
        model: input.model,
        model_target: input.model_target.unwrap_or_default(),
        compute_lease_id: input.compute_lease_id,
        compute_provider: input.compute_provider,
        estimated_compute_cost_usd: input.estimated_compute_cost_usd,
        fallback_model_target: input.fallback_model_target,
        created_at: ts,
        updated_at: ts,
    };

    sqlx::query(
        "INSERT INTO tasks (id, project_id, title, description, status, priority, \
         primary_agent, model, model_target, compute_lease_id, compute_provider, \
         estimated_compute_cost_usd, fallback_model_target, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&task.id)
    .bind(&task.project_id)
    .bind(&task.title)
    .bind(&task.description)
    .bind(task.status.as_str())
    .bind(task.priority.as_str())
    .bind(task.primary_agent.map(|a| a.as_str()))
    .bind(&task.model)
    .bind(task.model_target.as_str())
    .bind(&task.compute_lease_id)
    .bind(task.compute_provider.map(|provider| provider.as_str()))
    .bind(task.estimated_compute_cost_usd)
    .bind(task.fallback_model_target.map(|target| target.as_str()))
    .bind(task.created_at)
    .bind(task.updated_at)
    .execute(pool)
    .await?;

    Ok(task)
}

pub async fn list_for_project(pool: &SqlitePool, project_id: &str) -> Result<Vec<Task>, DbError> {
    let rows = sqlx::query_as::<_, TaskRow>(&format!(
        "{SELECT} WHERE project_id = ? ORDER BY created_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(Task::try_from).collect()
}

pub async fn list_for_status(
    pool: &SqlitePool,
    status: TaskStatus,
    limit: i64,
) -> Result<Vec<Task>, DbError> {
    let rows = sqlx::query_as::<_, TaskRow>(&format!(
        "{SELECT} WHERE status = ? ORDER BY updated_at ASC LIMIT ?"
    ))
    .bind(status.as_str())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(Task::try_from).collect()
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Task>, DbError> {
    let row = sqlx::query_as::<_, TaskRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(Task::try_from).transpose()
}

/// Reconcile tasks left `running` by a previous process: there is no live
/// session backing them after a restart, so move them to `paused` (resumable).
/// Returns the number reconciled.
pub async fn pause_orphaned_running(pool: &SqlitePool) -> Result<u64, DbError> {
    let res = sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE status = 'running'")
        .bind(TaskStatus::Paused.as_str())
        .bind(now())
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Apply a partial update and return the refreshed task.
pub async fn update(pool: &SqlitePool, id: &str, patch: TaskUpdate) -> Result<Task, DbError> {
    let mut task = get(pool, id).await?.ok_or(DbError::NotFound)?;

    if let Some(title) = patch.title {
        task.title = title;
    }
    if patch.description.is_some() {
        task.description = patch.description;
    }
    if let Some(status) = patch.status {
        task.status = status;
    }
    if let Some(priority) = patch.priority {
        task.priority = priority;
    }
    if patch.primary_agent.is_some() {
        task.primary_agent = patch.primary_agent;
    }
    if let Some(model) = patch.model {
        task.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(model_target) = patch.model_target {
        task.model_target = model_target;
    }
    if patch.compute_lease_id.is_some() {
        task.compute_lease_id = patch.compute_lease_id;
    }
    if patch.compute_provider.is_some() {
        task.compute_provider = patch.compute_provider;
    }
    if patch.estimated_compute_cost_usd.is_some() {
        task.estimated_compute_cost_usd = patch.estimated_compute_cost_usd;
    }
    if patch.fallback_model_target.is_some() {
        task.fallback_model_target = patch.fallback_model_target;
    }
    task.updated_at = now();

    sqlx::query(
        "UPDATE tasks SET title = ?, description = ?, status = ?, priority = ?, \
         primary_agent = ?, model = ?, model_target = ?, compute_lease_id = ?, \
         compute_provider = ?, estimated_compute_cost_usd = ?, fallback_model_target = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&task.title)
    .bind(&task.description)
    .bind(task.status.as_str())
    .bind(task.priority.as_str())
    .bind(task.primary_agent.map(|a| a.as_str()))
    .bind(&task.model)
    .bind(task.model_target.as_str())
    .bind(&task.compute_lease_id)
    .bind(task.compute_provider.map(|provider| provider.as_str()))
    .bind(task.estimated_compute_cost_usd)
    .bind(task.fallback_model_target.map(|target| target.as_str()))
    .bind(task.updated_at)
    .bind(&task.id)
    .execute(pool)
    .await?;

    Ok(task)
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

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
