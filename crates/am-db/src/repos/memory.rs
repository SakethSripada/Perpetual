use am_proto::{new_id, now, MemoryNote, MemoryNoteUpdate, NewMemoryNote};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: String,
    project_id: String,
    task_id: Option<String>,
    body: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<NoteRow> for MemoryNote {
    fn from(r: NoteRow) -> Self {
        MemoryNote {
            id: r.id,
            project_id: r.project_id,
            task_id: r.task_id,
            body: r.body,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const SELECT: &str =
    "SELECT id, project_id, task_id, body, created_at, updated_at FROM memory_notes";

pub async fn create(pool: &SqlitePool, input: NewMemoryNote) -> Result<MemoryNote, DbError> {
    let ts = now();
    let note = MemoryNote {
        id: new_id(),
        project_id: input.project_id,
        task_id: input.task_id,
        body: input.body,
        created_at: ts,
        updated_at: ts,
    };

    sqlx::query(
        "INSERT INTO memory_notes (id, project_id, task_id, body, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&note.id)
    .bind(&note.project_id)
    .bind(&note.task_id)
    .bind(&note.body)
    .bind(note.created_at)
    .bind(note.updated_at)
    .execute(pool)
    .await?;

    Ok(note)
}

/// Project-level notes only (those not attached to a specific task).
pub async fn list_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<MemoryNote>, DbError> {
    let rows = sqlx::query_as::<_, NoteRow>(&format!(
        "{SELECT} WHERE project_id = ? AND task_id IS NULL ORDER BY created_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Notes attached to a specific task.
pub async fn list_for_task(pool: &SqlitePool, task_id: &str) -> Result<Vec<MemoryNote>, DbError> {
    let rows = sqlx::query_as::<_, NoteRow>(&format!(
        "{SELECT} WHERE task_id = ? ORDER BY created_at DESC"
    ))
    .bind(task_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<MemoryNote>, DbError> {
    let row = sqlx::query_as::<_, NoteRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(Into::into))
}

pub async fn update(
    pool: &SqlitePool,
    id: &str,
    patch: MemoryNoteUpdate,
) -> Result<MemoryNote, DbError> {
    let mut note = get(pool, id).await?.ok_or(DbError::NotFound)?;
    if let Some(body) = patch.body {
        note.body = body;
    }
    note.updated_at = now();

    sqlx::query("UPDATE memory_notes SET body = ?, updated_at = ? WHERE id = ?")
        .bind(&note.body)
        .bind(note.updated_at)
        .bind(&note.id)
        .execute(pool)
        .await?;

    Ok(note)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM memory_notes WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
