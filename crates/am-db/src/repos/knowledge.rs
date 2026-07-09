use am_proto::{new_id, now, KnowledgeDoc, KnowledgeDocUpdate, NewKnowledgeDoc};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct DocRow {
    id: String,
    project_id: String,
    title: String,
    body: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<DocRow> for KnowledgeDoc {
    fn from(r: DocRow) -> Self {
        KnowledgeDoc {
            id: r.id,
            project_id: r.project_id,
            title: r.title,
            body: r.body,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const SELECT: &str =
    "SELECT id, project_id, title, body, created_at, updated_at FROM knowledge_docs";

pub async fn create(pool: &SqlitePool, input: NewKnowledgeDoc) -> Result<KnowledgeDoc, DbError> {
    let ts = now();
    let doc = KnowledgeDoc {
        id: new_id(),
        project_id: input.project_id,
        title: input.title,
        body: input.body,
        created_at: ts,
        updated_at: ts,
    };

    sqlx::query(
        "INSERT INTO knowledge_docs (id, project_id, title, body, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&doc.id)
    .bind(&doc.project_id)
    .bind(&doc.title)
    .bind(&doc.body)
    .bind(doc.created_at)
    .bind(doc.updated_at)
    .execute(pool)
    .await?;

    Ok(doc)
}

pub async fn list_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<KnowledgeDoc>, DbError> {
    let rows = sqlx::query_as::<_, DocRow>(&format!(
        "{SELECT} WHERE project_id = ? ORDER BY updated_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<KnowledgeDoc>, DbError> {
    let row = sqlx::query_as::<_, DocRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(Into::into))
}

/// Apply a partial update and return the refreshed document.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    patch: KnowledgeDocUpdate,
) -> Result<KnowledgeDoc, DbError> {
    let mut doc = get(pool, id).await?.ok_or(DbError::NotFound)?;

    if let Some(title) = patch.title {
        doc.title = title;
    }
    if let Some(body) = patch.body {
        doc.body = body;
    }
    doc.updated_at = now();

    sqlx::query("UPDATE knowledge_docs SET title = ?, body = ?, updated_at = ? WHERE id = ?")
        .bind(&doc.title)
        .bind(&doc.body)
        .bind(doc.updated_at)
        .bind(&doc.id)
        .execute(pool)
        .await?;

    Ok(doc)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM knowledge_docs WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
