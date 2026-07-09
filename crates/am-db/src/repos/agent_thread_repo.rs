use am_proto::{AgentThreadRepo, ExecutionBackend};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct ThreadRepoRow {
    thread_id: String,
    repo_id: String,
    repo_name: String,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    workspace_backend: String,
}

impl TryFrom<ThreadRepoRow> for AgentThreadRepo {
    type Error = DbError;

    fn try_from(r: ThreadRepoRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_id: r.thread_id,
            repo_id: r.repo_id,
            repo_name: r.repo_name,
            worktree_path: r.worktree_path,
            branch: r.branch,
            base_ref: r.base_ref,
            workspace_backend: ExecutionBackend::parse(&r.workspace_backend)
                .ok_or_else(|| DbError::InvalidEnum(r.workspace_backend.clone()))?,
        })
    }
}

pub async fn replace_repos(
    pool: &SqlitePool,
    thread_id: &str,
    repo_ids: &[String],
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM agent_thread_repos WHERE thread_id = ?")
        .bind(thread_id)
        .execute(pool)
        .await?;

    for repo_id in repo_ids {
        sqlx::query(
            "INSERT INTO agent_thread_repos (thread_id, repo_id, worktree_path, branch, base_ref, workspace_backend) \
             VALUES (?, ?, NULL, NULL, NULL, 'host')",
        )
        .bind(thread_id)
        .bind(repo_id)
        .execute(pool)
        .await?;
    }

    Ok(())
}

pub async fn upsert(
    pool: &SqlitePool,
    thread_id: &str,
    repo_id: &str,
    worktree_path: Option<&str>,
    branch: Option<&str>,
    base_ref: Option<&str>,
    workspace_backend: ExecutionBackend,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_thread_repos (thread_id, repo_id, worktree_path, branch, base_ref, workspace_backend) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(thread_id, repo_id) DO UPDATE SET \
           worktree_path = excluded.worktree_path, \
           branch = excluded.branch, \
           base_ref = excluded.base_ref, \
           workspace_backend = excluded.workspace_backend",
    )
    .bind(thread_id)
    .bind(repo_id)
    .bind(worktree_path)
    .bind(branch)
    .bind(base_ref)
    .bind(workspace_backend.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Vec<AgentThreadRepo>, DbError> {
    let rows = sqlx::query_as::<_, ThreadRepoRow>(
        "SELECT tr.thread_id, tr.repo_id, r.name AS repo_name, tr.worktree_path, tr.branch, tr.base_ref, \
         tr.workspace_backend \
         FROM agent_thread_repos tr \
         JOIN repos r ON r.id = tr.repo_id \
         WHERE tr.thread_id = ? ORDER BY r.name ASC",
    )
    .bind(thread_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(AgentThreadRepo::try_from).collect()
}
