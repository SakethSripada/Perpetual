use am_proto::{new_id, now, AgentKind, CloudHandoffTrigger, CloudRun, CloudRunStatus};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct CloudRunRow {
    id: String,
    thread_id: String,
    agent_kind: String,
    provider_task_id: Option<String>,
    url: Option<String>,
    env_id: Option<String>,
    branch: Option<String>,
    base_commit: Option<String>,
    launch_commit: Option<String>,
    status: String,
    trigger: String,
    launched_at: DateTime<Utc>,
    last_activity_at: Option<DateTime<Utc>>,
    last_seen_commit: Option<String>,
    reclaimed_at: Option<DateTime<Utc>>,
    failure_reason: Option<String>,
}

impl TryFrom<CloudRunRow> for CloudRun {
    type Error = DbError;
    fn try_from(r: CloudRunRow) -> Result<Self, DbError> {
        Ok(CloudRun {
            id: r.id,
            thread_id: r.thread_id,
            agent_kind: AgentKind::parse(&r.agent_kind)
                .ok_or_else(|| DbError::InvalidEnum(r.agent_kind.clone()))?,
            provider_task_id: r.provider_task_id,
            url: r.url,
            env_id: r.env_id,
            branch: r.branch,
            base_commit: r.base_commit,
            launch_commit: r.launch_commit,
            status: CloudRunStatus::parse(&r.status)
                .ok_or_else(|| DbError::InvalidEnum(r.status.clone()))?,
            trigger: CloudHandoffTrigger::parse(&r.trigger)
                .ok_or_else(|| DbError::InvalidEnum(r.trigger.clone()))?,
            launched_at: r.launched_at,
            last_activity_at: r.last_activity_at,
            last_seen_commit: r.last_seen_commit,
            reclaimed_at: r.reclaimed_at,
            failure_reason: r.failure_reason,
        })
    }
}

const SELECT: &str = "SELECT id, thread_id, agent_kind, provider_task_id, url, env_id, branch, \
     base_commit, launch_commit, status, trigger, launched_at, last_activity_at, \
     last_seen_commit, reclaimed_at, failure_reason FROM cloud_runs";

/// Input for recording a freshly launched cloud run.
pub struct NewCloudRun<'a> {
    pub thread_id: &'a str,
    pub agent_kind: AgentKind,
    pub provider_task_id: Option<&'a str>,
    pub url: Option<&'a str>,
    pub env_id: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub base_commit: Option<&'a str>,
    pub launch_commit: Option<&'a str>,
    pub trigger: CloudHandoffTrigger,
}

pub async fn create(pool: &SqlitePool, input: NewCloudRun<'_>) -> Result<CloudRun, DbError> {
    let run = CloudRun {
        id: new_id(),
        thread_id: input.thread_id.to_string(),
        agent_kind: input.agent_kind,
        provider_task_id: input.provider_task_id.map(str::to_string),
        url: input.url.map(str::to_string),
        env_id: input.env_id.map(str::to_string),
        branch: input.branch.map(str::to_string),
        base_commit: input.base_commit.map(str::to_string),
        launch_commit: input.launch_commit.map(str::to_string),
        status: CloudRunStatus::Provisioning,
        trigger: input.trigger,
        launched_at: now(),
        last_activity_at: None,
        last_seen_commit: input.launch_commit.map(str::to_string),
        reclaimed_at: None,
        failure_reason: None,
    };
    sqlx::query(
        "INSERT INTO cloud_runs (id, thread_id, agent_kind, provider_task_id, url, env_id, \
         branch, base_commit, launch_commit, status, trigger, launched_at, last_seen_commit) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run.id)
    .bind(&run.thread_id)
    .bind(run.agent_kind.as_str())
    .bind(&run.provider_task_id)
    .bind(&run.url)
    .bind(&run.env_id)
    .bind(&run.branch)
    .bind(&run.base_commit)
    .bind(&run.launch_commit)
    .bind(run.status.as_str())
    .bind(run.trigger.as_str())
    .bind(run.launched_at)
    .bind(&run.last_seen_commit)
    .execute(pool)
    .await?;
    Ok(run)
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<CloudRun>, DbError> {
    let row = sqlx::query_as::<_, CloudRunRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(CloudRun::try_from).transpose()
}

/// The newest still-active run for a thread, if any.
pub async fn active_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Option<CloudRun>, DbError> {
    let row = sqlx::query_as::<_, CloudRunRow>(&format!(
        "{SELECT} WHERE thread_id = ? AND status IN ('provisioning', 'running', 'stalled') \
         ORDER BY launched_at DESC LIMIT 1"
    ))
    .bind(thread_id)
    .fetch_optional(pool)
    .await?;
    row.map(CloudRun::try_from).transpose()
}

/// Every run that still needs monitoring, oldest first.
pub async fn list_active(pool: &SqlitePool) -> Result<Vec<CloudRun>, DbError> {
    let rows = sqlx::query_as::<_, CloudRunRow>(&format!(
        "{SELECT} WHERE status IN ('provisioning', 'running', 'stalled') ORDER BY launched_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(CloudRun::try_from).collect()
}

/// Full history for a thread, newest first.
pub async fn list_for_thread(pool: &SqlitePool, thread_id: &str) -> Result<Vec<CloudRun>, DbError> {
    let rows = sqlx::query_as::<_, CloudRunRow>(&format!(
        "{SELECT} WHERE thread_id = ? ORDER BY launched_at DESC"
    ))
    .bind(thread_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(CloudRun::try_from).collect()
}

pub async fn set_status(
    pool: &SqlitePool,
    id: &str,
    status: CloudRunStatus,
) -> Result<(), DbError> {
    sqlx::query("UPDATE cloud_runs SET status = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record provider identifiers once the launch output has been parsed.
pub async fn set_provider_ref(
    pool: &SqlitePool,
    id: &str,
    provider_task_id: Option<&str>,
    url: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE cloud_runs SET provider_task_id = COALESCE(?, provider_task_id), \
         url = COALESCE(?, url) WHERE id = ?",
    )
    .bind(provider_task_id)
    .bind(url)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Bump activity tracking after the monitor observed progress.
pub async fn record_activity(
    pool: &SqlitePool,
    id: &str,
    last_seen_commit: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE cloud_runs SET last_activity_at = ?, \
         last_seen_commit = COALESCE(?, last_seen_commit) WHERE id = ?",
    )
    .bind(now())
    .bind(last_seen_commit)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Close out a run. Terminal `status` should be `Reclaimed` (or `Failed` when
/// reclaim itself was impossible).
pub async fn close(
    pool: &SqlitePool,
    id: &str,
    status: CloudRunStatus,
    failure_reason: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE cloud_runs SET status = ?, reclaimed_at = ?, failure_reason = ? WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(now())
    .bind(failure_reason)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
