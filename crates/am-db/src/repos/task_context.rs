use am_proto::{now, Task, TaskContext, TaskContextUpdate, TaskHandoff};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct TaskContextRow {
    task_id: String,
    objective: String,
    requirements: String,
    decisions: String,
    progress: String,
    open_questions: String,
    next_actions: String,
    updated_at: DateTime<Utc>,
}

impl From<TaskContextRow> for TaskContext {
    fn from(r: TaskContextRow) -> Self {
        TaskContext {
            task_id: r.task_id,
            objective: r.objective,
            requirements: r.requirements,
            decisions: r.decisions,
            progress: r.progress,
            open_questions: r.open_questions,
            next_actions: r.next_actions,
            updated_at: r.updated_at,
        }
    }
}

const SELECT: &str = "SELECT task_id, objective, requirements, decisions, progress, \
    open_questions, next_actions, updated_at FROM task_context";

pub async fn get(pool: &SqlitePool, task_id: &str) -> Result<Option<TaskContext>, DbError> {
    let row = sqlx::query_as::<_, TaskContextRow>(&format!("{SELECT} WHERE task_id = ?"))
        .bind(task_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(TaskContext::from))
}

pub async fn ensure_for_task(pool: &SqlitePool, task: &Task) -> Result<TaskContext, DbError> {
    if let Some(context) = get(pool, &task.id).await? {
        return Ok(context);
    }

    let context = TaskContext {
        task_id: task.id.clone(),
        objective: task.title.clone(),
        requirements: task.description.clone().unwrap_or_default(),
        decisions: String::new(),
        progress: String::new(),
        open_questions: String::new(),
        next_actions: String::new(),
        updated_at: now(),
    };
    upsert(pool, &context).await
}

pub async fn upsert(pool: &SqlitePool, context: &TaskContext) -> Result<TaskContext, DbError> {
    let updated_at = now();
    sqlx::query(
        "INSERT INTO task_context (task_id, objective, requirements, decisions, progress, \
         open_questions, next_actions, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(task_id) DO UPDATE SET objective = excluded.objective, \
         requirements = excluded.requirements, decisions = excluded.decisions, \
         progress = excluded.progress, open_questions = excluded.open_questions, \
         next_actions = excluded.next_actions, updated_at = excluded.updated_at",
    )
    .bind(&context.task_id)
    .bind(&context.objective)
    .bind(&context.requirements)
    .bind(&context.decisions)
    .bind(&context.progress)
    .bind(&context.open_questions)
    .bind(&context.next_actions)
    .bind(updated_at)
    .execute(pool)
    .await?;

    get(pool, &context.task_id).await?.ok_or(DbError::NotFound)
}

pub async fn update(
    pool: &SqlitePool,
    task_id: &str,
    patch: TaskContextUpdate,
) -> Result<TaskContext, DbError> {
    let mut context = get(pool, task_id).await?.ok_or(DbError::NotFound)?;
    if let Some(objective) = patch.objective {
        context.objective = objective;
    }
    if let Some(requirements) = patch.requirements {
        context.requirements = requirements;
    }
    if let Some(decisions) = patch.decisions {
        context.decisions = decisions;
    }
    if let Some(progress) = patch.progress {
        context.progress = progress;
    }
    if let Some(open_questions) = patch.open_questions {
        context.open_questions = open_questions;
    }
    if let Some(next_actions) = patch.next_actions {
        context.next_actions = next_actions;
    }
    upsert(pool, &context).await
}

// ---- Handoff archive ------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TaskHandoffRow {
    id: String,
    task_id: String,
    session_id: String,
    agent: String,
    status: String,
    summary: String,
    changed_files_json: String,
    next_actions: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<TaskHandoffRow> for TaskHandoff {
    type Error = DbError;

    fn try_from(row: TaskHandoffRow) -> Result<Self, Self::Error> {
        Ok(TaskHandoff {
            agent: am_proto::AgentKind::parse(&row.agent)
                .ok_or_else(|| DbError::InvalidEnum(row.agent.clone()))?,
            changed_files: serde_json::from_str(&row.changed_files_json).unwrap_or_default(),
            id: row.id,
            task_id: row.task_id,
            session_id: row.session_id,
            status: row.status,
            summary: row.summary,
            next_actions: row.next_actions,
            created_at: row.created_at,
        })
    }
}

const HANDOFF_SELECT: &str = "SELECT id, task_id, session_id, agent, status, summary, \
    changed_files_json, next_actions, created_at FROM task_handoffs";

/// Archive a session handoff (append-only; rendered progress is a bounded
/// window over these rows).
pub async fn insert_handoff(pool: &SqlitePool, handoff: &TaskHandoff) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO task_handoffs \
         (id, task_id, session_id, agent, status, summary, changed_files_json, next_actions, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&handoff.id)
    .bind(&handoff.task_id)
    .bind(&handoff.session_id)
    .bind(handoff.agent.as_str())
    .bind(&handoff.status)
    .bind(&handoff.summary)
    .bind(serde_json::to_string(&handoff.changed_files).unwrap_or_else(|_| "[]".into()))
    .bind(&handoff.next_actions)
    .bind(handoff.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Latest handoffs for one task, newest first.
pub async fn list_handoffs(
    pool: &SqlitePool,
    task_id: &str,
    limit: i64,
) -> Result<Vec<TaskHandoff>, DbError> {
    let rows = sqlx::query_as::<_, TaskHandoffRow>(&format!(
        "{HANDOFF_SELECT} WHERE task_id = ? ORDER BY created_at DESC, rowid DESC LIMIT ?"
    ))
    .bind(task_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TaskHandoff::try_from).collect()
}

/// The most recent handoff for a task, if any.
pub async fn latest_handoff(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<TaskHandoff>, DbError> {
    Ok(list_handoffs(pool, task_id, 1).await?.into_iter().next())
}
