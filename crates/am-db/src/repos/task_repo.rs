use am_proto::ExecutionBackend;
use sqlx::SqlitePool;

use crate::DbError;

/// A task↔repo association including its isolated workspace and base commit.
#[derive(Debug, Clone)]
pub struct TaskRepoLink {
    pub task_id: String,
    pub repo_id: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub workspace_backend: ExecutionBackend,
}

#[derive(sqlx::FromRow)]
struct TaskRepoLinkRow {
    task_id: String,
    repo_id: String,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    workspace_backend: String,
}

impl TryFrom<TaskRepoLinkRow> for TaskRepoLink {
    type Error = DbError;

    fn try_from(row: TaskRepoLinkRow) -> Result<Self, Self::Error> {
        Ok(Self {
            task_id: row.task_id,
            repo_id: row.repo_id,
            worktree_path: row.worktree_path,
            branch: row.branch,
            base_ref: row.base_ref,
            workspace_backend: ExecutionBackend::parse(&row.workspace_backend)
                .ok_or_else(|| DbError::InvalidEnum(row.workspace_backend.clone()))?,
        })
    }
}

pub async fn upsert(pool: &SqlitePool, link: &TaskRepoLink) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO task_repos (task_id, repo_id, worktree_path, branch, base_ref, workspace_backend) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(task_id, repo_id) DO UPDATE SET \
           worktree_path = excluded.worktree_path, \
           branch = excluded.branch, \
           base_ref = excluded.base_ref, \
           workspace_backend = excluded.workspace_backend",
    )
    .bind(&link.task_id)
    .bind(&link.repo_id)
    .bind(&link.worktree_path)
    .bind(&link.branch)
    .bind(&link.base_ref)
    .bind(link.workspace_backend.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn replace_repo(pool: &SqlitePool, task_id: &str, repo_id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM task_repos WHERE task_id = ?")
        .bind(task_id)
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO task_repos (task_id, repo_id, worktree_path, branch, base_ref, workspace_backend) \
         VALUES (?, ?, NULL, NULL, NULL, 'host')",
    )
    .bind(task_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn clear_for_task(pool: &SqlitePool, task_id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM task_repos WHERE task_id = ?")
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The (single, for M1) repo link for a task.
pub async fn get_for_task(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<TaskRepoLink>, DbError> {
    let row = sqlx::query_as::<_, TaskRepoLinkRow>(
        "SELECT task_id, repo_id, worktree_path, branch, base_ref, workspace_backend FROM task_repos \
         WHERE task_id = ? LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?;
    row.map(TaskRepoLink::try_from).transpose()
}
