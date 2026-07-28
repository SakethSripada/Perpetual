use am_proto::{
    now, CollaborationAgentCapability, CollaborationAssignment, CollaborationAssignmentStatus,
    CollaborationChangeSet, CollaborationChangeStatus, CollaborationDevice, ExecutionBackend,
};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct DeviceRow {
    id: String,
    name: String,
    hostname: String,
    platform: String,
    extension_version: String,
    capabilities_json: String,
    last_seen_at: DateTime<Utc>,
    paired_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    active_assignments: i64,
}

impl TryFrom<DeviceRow> for CollaborationDevice {
    type Error = DbError;

    fn try_from(row: DeviceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            hostname: row.hostname,
            platform: row.platform,
            extension_version: row.extension_version,
            capabilities: serde_json::from_str::<Vec<CollaborationAgentCapability>>(
                &row.capabilities_json,
            )
            .map_err(|err| DbError::Serde(err.to_string()))?,
            last_seen_at: row.last_seen_at,
            paired_at: row.paired_at,
            revoked_at: row.revoked_at,
            active_assignments: row.active_assignments,
        })
    }
}

const DEVICE_SELECT: &str = "SELECT d.id, d.name, d.hostname, d.platform, d.extension_version, \
    d.capabilities_json, d.last_seen_at, d.paired_at, d.revoked_at, \
    (SELECT COUNT(*) FROM collaboration_assignments a WHERE a.device_id = d.id \
      AND a.status IN ('queued', 'running', 'review')) AS active_assignments \
    FROM collaboration_devices d";

pub async fn upsert_device(
    pool: &SqlitePool,
    input: &am_proto::RegisterCollaborationDevice,
) -> Result<CollaborationDevice, DbError> {
    let ts = now();
    let capabilities_json = serde_json::to_string(&input.capabilities)
        .map_err(|err| DbError::Serde(err.to_string()))?;
    sqlx::query(
        "INSERT INTO collaboration_devices \
         (id, name, hostname, platform, extension_version, capabilities_json, last_seen_at, paired_at, revoked_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, hostname = excluded.hostname, \
         platform = excluded.platform, extension_version = excluded.extension_version, \
         capabilities_json = excluded.capabilities_json, last_seen_at = excluded.last_seen_at",
    )
    .bind(&input.id)
    .bind(&input.name)
    .bind(&input.hostname)
    .bind(&input.platform)
    .bind(&input.extension_version)
    .bind(capabilities_json)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    get_device(pool, &input.id).await?.ok_or(DbError::NotFound)
}

pub async fn heartbeat_device(
    pool: &SqlitePool,
    input: &am_proto::RegisterCollaborationDevice,
) -> Result<CollaborationDevice, DbError> {
    upsert_device(pool, input).await
}

pub async fn get_device(
    pool: &SqlitePool,
    device_id: &str,
) -> Result<Option<CollaborationDevice>, DbError> {
    let row = sqlx::query_as::<_, DeviceRow>(&format!("{DEVICE_SELECT} WHERE d.id = ?"))
        .bind(device_id)
        .fetch_optional(pool)
        .await?;
    row.map(CollaborationDevice::try_from).transpose()
}

pub async fn list_devices(pool: &SqlitePool) -> Result<Vec<CollaborationDevice>, DbError> {
    let rows = sqlx::query_as::<_, DeviceRow>(&format!(
        "{DEVICE_SELECT} ORDER BY d.revoked_at IS NOT NULL, d.last_seen_at DESC"
    ))
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(CollaborationDevice::try_from)
        .collect()
}

pub async fn revoke_device(pool: &SqlitePool, device_id: &str) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    let ts = now();
    sqlx::query("UPDATE collaboration_devices SET revoked_at = ? WHERE id = ?")
        .bind(ts)
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE collaboration_assignments SET status = 'cancelled', finished_at = ?, \
         lease_token_hash = NULL, lease_expires_at = NULL, error = 'device access revoked' \
         WHERE device_id = ? AND status IN ('queued', 'running')",
    )
    .bind(ts)
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct AssignmentRow {
    id: String,
    thread_id: String,
    turn_id: String,
    device_id: String,
    device_name: String,
    agent_kind: String,
    permission: String,
    execution_backend: String,
    prompt: String,
    status: String,
    lease_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

impl TryFrom<AssignmentRow> for CollaborationAssignment {
    type Error = DbError;

    fn try_from(row: AssignmentRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            thread_id: row.thread_id,
            turn_id: row.turn_id,
            device_id: row.device_id,
            device_name: row.device_name,
            agent: am_proto::AgentKind::parse(&row.agent_kind)
                .ok_or_else(|| DbError::InvalidEnum(row.agent_kind.clone()))?,
            permission: row.permission,
            execution_backend: ExecutionBackend::parse(&row.execution_backend)
                .ok_or_else(|| DbError::InvalidEnum(row.execution_backend.clone()))?,
            prompt: row.prompt,
            status: CollaborationAssignmentStatus::parse(&row.status)
                .ok_or_else(|| DbError::InvalidEnum(row.status.clone()))?,
            lease_expires_at: row.lease_expires_at,
            created_at: row.created_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
            error: row.error,
        })
    }
}

const ASSIGNMENT_SELECT: &str = "SELECT a.id, a.thread_id, a.turn_id, a.device_id, \
    d.name AS device_name, a.agent_kind, a.permission, a.execution_backend, a.prompt, \
    a.status, a.lease_expires_at, a.created_at, a.started_at, a.finished_at, a.error \
    FROM collaboration_assignments a JOIN collaboration_devices d ON d.id = a.device_id";

pub async fn insert_assignment(
    pool: &SqlitePool,
    assignment: &CollaborationAssignment,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO collaboration_assignments \
         (id, thread_id, turn_id, device_id, agent_kind, permission, execution_backend, prompt, \
          status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&assignment.id)
    .bind(&assignment.thread_id)
    .bind(&assignment.turn_id)
    .bind(&assignment.device_id)
    .bind(assignment.agent.as_str())
    .bind(&assignment.permission)
    .bind(assignment.execution_backend.as_str())
    .bind(&assignment.prompt)
    .bind(assignment.status.as_str())
    .bind(assignment.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let row = sqlx::query_as::<_, AssignmentRow>(&format!("{ASSIGNMENT_SELECT} WHERE a.id = ?"))
        .bind(assignment_id)
        .fetch_optional(pool)
        .await?;
    row.map(CollaborationAssignment::try_from).transpose()
}

pub async fn list_assignments(
    pool: &SqlitePool,
    device_id: Option<&str>,
    active_only: bool,
) -> Result<Vec<CollaborationAssignment>, DbError> {
    let mut query = format!("{ASSIGNMENT_SELECT} WHERE 1 = 1");
    if device_id.is_some() {
        query.push_str(" AND a.device_id = ?");
    }
    if active_only {
        query.push_str(" AND a.status IN ('queued', 'running', 'review')");
    }
    query.push_str(" ORDER BY a.created_at DESC LIMIT 250");
    let mut q = sqlx::query_as::<_, AssignmentRow>(&query);
    if let Some(device_id) = device_id {
        q = q.bind(device_id);
    }
    let rows = q.fetch_all(pool).await?;
    rows.into_iter()
        .map(CollaborationAssignment::try_from)
        .collect()
}

/// Atomically claim a queued assignment and install a hashed, expiring lease.
pub async fn claim_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
    device_id: &str,
    lease_token_hash: &str,
    lease_expires_at: DateTime<Utc>,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let ts = now();
    let result = sqlx::query(
        "UPDATE collaboration_assignments SET status = 'running', lease_token_hash = ?, \
         lease_expires_at = ?, started_at = COALESCE(started_at, ?) \
         WHERE id = ? AND device_id = ? AND status = 'queued' \
         AND EXISTS (SELECT 1 FROM collaboration_devices d WHERE d.id = ? AND d.revoked_at IS NULL)",
    )
    .bind(lease_token_hash)
    .bind(lease_expires_at)
    .bind(ts)
    .bind(assignment_id)
    .bind(device_id)
    .bind(device_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_assignment(pool, assignment_id).await
}

pub async fn validate_lease(
    pool: &SqlitePool,
    assignment_id: &str,
    lease_token_hash: &str,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let row = sqlx::query_as::<_, AssignmentRow>(&format!(
        "{ASSIGNMENT_SELECT} WHERE a.id = ? AND a.lease_token_hash = ? \
         AND a.status = 'running' AND a.lease_expires_at > ? AND d.revoked_at IS NULL"
    ))
    .bind(assignment_id)
    .bind(lease_token_hash)
    .bind(now())
    .fetch_optional(pool)
    .await?;
    row.map(CollaborationAssignment::try_from).transpose()
}

pub async fn renew_lease(
    pool: &SqlitePool,
    assignment_id: &str,
    lease_token_hash: &str,
    lease_expires_at: DateTime<Utc>,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let result = sqlx::query(
        "UPDATE collaboration_assignments SET lease_expires_at = ? \
         WHERE id = ? AND lease_token_hash = ? AND status = 'running' \
         AND lease_expires_at > ?",
    )
    .bind(lease_expires_at)
    .bind(assignment_id)
    .bind(lease_token_hash)
    .bind(now())
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_assignment(pool, assignment_id).await
}

pub async fn finish_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
    lease_token_hash: &str,
    status: CollaborationAssignmentStatus,
    error: Option<&str>,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let result = sqlx::query(
        "UPDATE collaboration_assignments SET status = ?, finished_at = ?, error = ?, \
         lease_token_hash = NULL, lease_expires_at = NULL \
         WHERE id = ? AND lease_token_hash = ? AND status = 'running' AND lease_expires_at > ?",
    )
    .bind(status.as_str())
    .bind(now())
    .bind(error)
    .bind(assignment_id)
    .bind(lease_token_hash)
    .bind(now())
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_assignment(pool, assignment_id).await
}

pub async fn cancel_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
) -> Result<Option<CollaborationAssignment>, DbError> {
    let result = sqlx::query(
        "UPDATE collaboration_assignments SET status = 'cancelled', finished_at = ?, \
         lease_token_hash = NULL, lease_expires_at = NULL, error = 'cancelled by user' \
         WHERE id = ? AND status IN ('queued', 'running', 'review')",
    )
    .bind(now())
    .bind(assignment_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_assignment(pool, assignment_id).await
}

pub async fn expire_stale_assignments(
    pool: &SqlitePool,
) -> Result<Vec<CollaborationAssignment>, DbError> {
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM collaboration_assignments WHERE status = 'running' \
         AND lease_expires_at <= ?",
    )
    .bind(now())
    .fetch_all(pool)
    .await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    for id in &ids {
        sqlx::query(
            "UPDATE collaboration_assignments SET status = 'lease_expired', finished_at = ?, \
             lease_token_hash = NULL, lease_expires_at = NULL, error = 'device lease expired' \
             WHERE id = ? AND status = 'running'",
        )
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    }
    let mut out = Vec::new();
    for id in ids {
        if let Some(assignment) = get_assignment(pool, &id).await? {
            out.push(assignment);
        }
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct ChangeSetRow {
    id: String,
    assignment_id: String,
    thread_id: String,
    device_id: String,
    repo_id: String,
    repo_name: String,
    base_ref: Option<String>,
    files_json: String,
    patch: String,
    patch_sha256: String,
    status: String,
    conflict_files_json: String,
    created_at: DateTime<Utc>,
    applied_at: Option<DateTime<Utc>>,
}

impl TryFrom<ChangeSetRow> for CollaborationChangeSet {
    type Error = DbError;

    fn try_from(row: ChangeSetRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            assignment_id: row.assignment_id,
            thread_id: row.thread_id,
            device_id: row.device_id,
            repo_id: row.repo_id,
            repo_name: row.repo_name,
            base_ref: row.base_ref,
            files: serde_json::from_str(&row.files_json)
                .map_err(|err| DbError::Serde(err.to_string()))?,
            patch: row.patch,
            patch_sha256: row.patch_sha256,
            status: CollaborationChangeStatus::parse(&row.status)
                .ok_or_else(|| DbError::InvalidEnum(row.status.clone()))?,
            conflict_files: serde_json::from_str(&row.conflict_files_json)
                .map_err(|err| DbError::Serde(err.to_string()))?,
            created_at: row.created_at,
            applied_at: row.applied_at,
        })
    }
}

const CHANGE_SELECT: &str = "SELECT c.id, c.assignment_id, c.thread_id, c.device_id, c.repo_id, \
    r.name AS repo_name, c.base_ref, c.files_json, c.patch, c.patch_sha256, c.status, \
    c.conflict_files_json, c.created_at, c.applied_at FROM collaboration_change_sets c \
    JOIN repos r ON r.id = c.repo_id";

pub async fn insert_change_set(
    pool: &SqlitePool,
    change: &CollaborationChangeSet,
) -> Result<CollaborationChangeSet, DbError> {
    let files_json =
        serde_json::to_string(&change.files).map_err(|err| DbError::Serde(err.to_string()))?;
    let conflicts_json = serde_json::to_string(&change.conflict_files)
        .map_err(|err| DbError::Serde(err.to_string()))?;
    sqlx::query(
        "INSERT INTO collaboration_change_sets \
         (id, assignment_id, thread_id, device_id, repo_id, base_ref, files_json, patch, \
          patch_sha256, status, conflict_files_json, created_at, applied_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&change.id)
    .bind(&change.assignment_id)
    .bind(&change.thread_id)
    .bind(&change.device_id)
    .bind(&change.repo_id)
    .bind(&change.base_ref)
    .bind(files_json)
    .bind(&change.patch)
    .bind(&change.patch_sha256)
    .bind(change.status.as_str())
    .bind(conflicts_json)
    .bind(change.created_at)
    .bind(change.applied_at)
    .execute(pool)
    .await?;
    get_change_set(pool, &change.id)
        .await?
        .ok_or(DbError::NotFound)
}

pub async fn get_change_set(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<CollaborationChangeSet>, DbError> {
    let row = sqlx::query_as::<_, ChangeSetRow>(&format!("{CHANGE_SELECT} WHERE c.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(CollaborationChangeSet::try_from).transpose()
}

pub async fn list_change_sets(
    pool: &SqlitePool,
    thread_id: Option<&str>,
) -> Result<Vec<CollaborationChangeSet>, DbError> {
    let query = match thread_id {
        Some(_) => format!("{CHANGE_SELECT} WHERE c.thread_id = ? ORDER BY c.created_at DESC"),
        None => format!("{CHANGE_SELECT} ORDER BY c.created_at DESC LIMIT 250"),
    };
    let mut q = sqlx::query_as::<_, ChangeSetRow>(&query);
    if let Some(thread_id) = thread_id {
        q = q.bind(thread_id);
    }
    let rows = q.fetch_all(pool).await?;
    rows.into_iter()
        .map(CollaborationChangeSet::try_from)
        .collect()
}

pub async fn update_change_status(
    pool: &SqlitePool,
    id: &str,
    status: CollaborationChangeStatus,
    conflict_files: &[String],
) -> Result<Option<CollaborationChangeSet>, DbError> {
    let conflicts_json =
        serde_json::to_string(conflict_files).map_err(|err| DbError::Serde(err.to_string()))?;
    let applied_at = matches!(
        status,
        CollaborationChangeStatus::Applied | CollaborationChangeStatus::AppliedWithOverwrite
    )
    .then(now);
    sqlx::query(
        "UPDATE collaboration_change_sets SET status = ?, conflict_files_json = ?, applied_at = ? \
         WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(conflicts_json)
    .bind(applied_at)
    .bind(id)
    .execute(pool)
    .await?;
    get_change_set(pool, id).await
}
