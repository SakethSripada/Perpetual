use am_proto::{new_id, now, NewProject, Project};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct ProjectRow {
    id: String,
    name: String,
    description: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Project {
            id: r.id,
            name: r.name,
            description: r.description,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const SELECT: &str = "SELECT id, name, description, created_at, updated_at FROM projects";

pub async fn create(pool: &SqlitePool, input: NewProject) -> Result<Project, DbError> {
    let ts = now();
    let project = Project {
        id: new_id(),
        name: input.name,
        description: input.description,
        created_at: ts,
        updated_at: ts,
    };

    sqlx::query(
        "INSERT INTO projects (id, name, description, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&project.id)
    .bind(&project.name)
    .bind(&project.description)
    .bind(project.created_at)
    .bind(project.updated_at)
    .execute(pool)
    .await?;

    Ok(project)
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Project>, DbError> {
    let rows = sqlx::query_as::<_, ProjectRow>(&format!("{SELECT} ORDER BY created_at DESC"))
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Project>, DbError> {
    let row = sqlx::query_as::<_, ProjectRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(Into::into))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
