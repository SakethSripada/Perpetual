use am_proto::{new_id, now, ComputeLease, ComputeLeaseStatus, ComputeProviderKind};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct ComputeLeaseRow {
    lease_json: String,
}

#[derive(Debug, Clone)]
pub struct ComputeLeaseEventInput {
    pub lease_id: String,
    pub status: ComputeLeaseStatus,
    pub message: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ComputeLeaseEventRow {
    pub id: String,
    pub lease_id: String,
    pub status: String,
    pub message: Option<String>,
    pub payload_json: String,
    pub ts: DateTime<Utc>,
}

pub async fn upsert_lease(pool: &SqlitePool, lease: &ComputeLease) -> Result<(), DbError> {
    let lease_json = serde_json::to_string(lease).map_err(|err| DbError::Serde(err.to_string()))?;
    let fallback_target_json = lease
        .fallback_target
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|err| DbError::Serde(err.to_string()))?;
    sqlx::query(
        "INSERT INTO compute_leases (
            id, quote_id, provider, provider_instance_id, model_id, model_label, status,
            region, gpu_summary, price_per_hour_usd, max_compute_usd, estimated_cost_usd,
            endpoint_base_url, endpoint_token_configured, fallback_target_json, status_message,
            started_at, ready_at, expires_at, terminated_at, lease_json, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            quote_id = excluded.quote_id,
            provider = excluded.provider,
            provider_instance_id = excluded.provider_instance_id,
            model_id = excluded.model_id,
            model_label = excluded.model_label,
            status = excluded.status,
            region = excluded.region,
            gpu_summary = excluded.gpu_summary,
            price_per_hour_usd = excluded.price_per_hour_usd,
            max_compute_usd = excluded.max_compute_usd,
            estimated_cost_usd = excluded.estimated_cost_usd,
            endpoint_base_url = excluded.endpoint_base_url,
            endpoint_token_configured = excluded.endpoint_token_configured,
            fallback_target_json = excluded.fallback_target_json,
            status_message = excluded.status_message,
            started_at = excluded.started_at,
            ready_at = excluded.ready_at,
            expires_at = excluded.expires_at,
            terminated_at = excluded.terminated_at,
            lease_json = excluded.lease_json,
            updated_at = excluded.updated_at",
    )
    .bind(&lease.id)
    .bind(&lease.quote_id)
    .bind(lease.provider.as_str())
    .bind(&lease.provider_instance_id)
    .bind(&lease.model_id)
    .bind(&lease.model_label)
    .bind(lease.status.as_str())
    .bind(&lease.region)
    .bind(&lease.gpu_summary)
    .bind(lease.price_per_hour_usd)
    .bind(lease.max_compute_usd)
    .bind(lease.estimated_cost_usd)
    .bind(&lease.endpoint_base_url)
    .bind(lease.endpoint_token_configured)
    .bind(&fallback_target_json)
    .bind(&lease.status_message)
    .bind(lease.started_at)
    .bind(lease.ready_at)
    .bind(lease.expires_at)
    .bind(lease.terminated_at)
    .bind(&lease_json)
    .bind(lease.created_at)
    .bind(lease.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_lease(pool: &SqlitePool, id: &str) -> Result<Option<ComputeLease>, DbError> {
    let row =
        sqlx::query_as::<_, ComputeLeaseRow>("SELECT lease_json FROM compute_leases WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    row.map(|row| parse_lease(&row.lease_json)).transpose()
}

pub async fn list_active_leases(pool: &SqlitePool) -> Result<Vec<ComputeLease>, DbError> {
    let rows = sqlx::query_as::<_, ComputeLeaseRow>(
        "SELECT lease_json FROM compute_leases
         WHERE status NOT IN ('expired', 'terminated', 'failed')
         ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| parse_lease(&row.lease_json))
        .collect()
}

pub async fn list_ready_compatible_leases(
    pool: &SqlitePool,
    model_id: &str,
    provider: Option<ComputeProviderKind>,
) -> Result<Vec<ComputeLease>, DbError> {
    let mut leases = list_active_leases(pool).await?;
    leases.retain(|lease| {
        lease.model_id == model_id
            && lease.status == ComputeLeaseStatus::Ready
            && provider
                .map(|provider| provider == lease.provider)
                .unwrap_or(true)
    });
    Ok(leases)
}

pub async fn record_event(pool: &SqlitePool, input: ComputeLeaseEventInput) -> Result<(), DbError> {
    let payload_json =
        serde_json::to_string(&input.payload).map_err(|err| DbError::Serde(err.to_string()))?;
    sqlx::query(
        "INSERT INTO compute_lease_events (id, lease_id, status, message, payload_json, ts)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(input.lease_id)
    .bind(input.status.as_str())
    .bind(input.message)
    .bind(payload_json)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_events(
    pool: &SqlitePool,
    lease_id: &str,
) -> Result<Vec<ComputeLeaseEventRow>, DbError> {
    sqlx::query_as::<_, ComputeLeaseEventRow>(
        "SELECT id, lease_id, status, message, payload_json, ts
         FROM compute_lease_events WHERE lease_id = ? ORDER BY ts ASC",
    )
    .bind(lease_id)
    .fetch_all(pool)
    .await
    .map_err(DbError::from)
}

fn parse_lease(raw: &str) -> Result<ComputeLease, DbError> {
    serde_json::from_str::<ComputeLease>(raw).map_err(|err| DbError::Serde(err.to_string()))
}
