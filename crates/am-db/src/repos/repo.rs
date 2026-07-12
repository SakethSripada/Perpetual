use am_proto::{new_id, now, Repo, RepoKind};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct RepoRow {
    id: String,
    project_id: String,
    name: String,
    kind: String,
    local_path: Option<String>,
    remote_url: Option<String>,
    default_branch: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<RepoRow> for Repo {
    type Error = DbError;
    fn try_from(r: RepoRow) -> Result<Self, DbError> {
        let kind = RepoKind::parse(&r.kind).ok_or_else(|| DbError::InvalidEnum(r.kind.clone()))?;
        Ok(Repo {
            id: r.id,
            project_id: r.project_id,
            name: r.name,
            kind,
            local_path: r.local_path,
            remote_url: r.remote_url,
            default_branch: r.default_branch,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

const SELECT: &str = "SELECT id, project_id, name, kind, local_path, remote_url, \
    default_branch, created_at, updated_at FROM repos";

pub async fn create_local(
    pool: &SqlitePool,
    project_id: &str,
    name: &str,
    local_path: &str,
    default_branch: &str,
) -> Result<Repo, DbError> {
    let ts = now();
    let repo = Repo {
        id: new_id(),
        project_id: project_id.to_string(),
        name: name.to_string(),
        kind: RepoKind::Local,
        local_path: Some(local_path.to_string()),
        remote_url: None,
        default_branch: default_branch.to_string(),
        created_at: ts,
        updated_at: ts,
    };
    sqlx::query(
        "INSERT INTO repos (id, project_id, name, kind, local_path, remote_url, \
         default_branch, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&repo.id)
    .bind(&repo.project_id)
    .bind(&repo.name)
    .bind(repo.kind.as_str())
    .bind(&repo.local_path)
    .bind(&repo.remote_url)
    .bind(&repo.default_branch)
    .bind(repo.created_at)
    .bind(repo.updated_at)
    .execute(pool)
    .await?;
    Ok(repo)
}

pub async fn create_github(
    pool: &SqlitePool,
    project_id: &str,
    name: &str,
    local_path: &str,
    remote_url: &str,
    default_branch: &str,
) -> Result<Repo, DbError> {
    let ts = now();
    let repo = Repo {
        id: new_id(),
        project_id: project_id.to_string(),
        name: name.to_string(),
        kind: RepoKind::GitHub,
        local_path: Some(local_path.to_string()),
        remote_url: Some(remote_url.to_string()),
        default_branch: default_branch.to_string(),
        created_at: ts,
        updated_at: ts,
    };
    sqlx::query(
        "INSERT INTO repos (id, project_id, name, kind, local_path, remote_url, \
         default_branch, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&repo.id)
    .bind(&repo.project_id)
    .bind(&repo.name)
    .bind(repo.kind.as_str())
    .bind(&repo.local_path)
    .bind(&repo.remote_url)
    .bind(&repo.default_branch)
    .bind(repo.created_at)
    .bind(repo.updated_at)
    .execute(pool)
    .await?;
    Ok(repo)
}

pub async fn list_for_project(pool: &SqlitePool, project_id: &str) -> Result<Vec<Repo>, DbError> {
    let rows = sqlx::query_as::<_, RepoRow>(&format!(
        "{SELECT} WHERE project_id = ? ORDER BY created_at ASC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(Repo::try_from).collect()
}

pub async fn get_by_project_remote(
    pool: &SqlitePool,
    project_id: &str,
    remote_url: &str,
) -> Result<Option<Repo>, DbError> {
    let row =
        sqlx::query_as::<_, RepoRow>(&format!("{SELECT} WHERE project_id = ? AND remote_url = ?"))
            .bind(project_id)
            .bind(remote_url)
            .fetch_optional(pool)
            .await?;
    row.map(Repo::try_from).transpose()
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Repo>, DbError> {
    let row = sqlx::query_as::<_, RepoRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(Repo::try_from).transpose()
}

/// Remove a connection. Thread/task/work-node links cascade.
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, DbError> {
    let result = sqlx::query("DELETE FROM repos WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_for_project(pool: &SqlitePool, project_id: &str) -> Result<u64, DbError> {
    let result = sqlx::query("DELETE FROM repos WHERE project_id = ?")
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
