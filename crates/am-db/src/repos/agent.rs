use am_proto::{now, AgentKind, AvailabilityState};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct AgentRecord {
    pub kind: AgentKind,
    pub install_status: String,
    pub version: Option<String>,
    pub availability: AvailabilityState,
    pub reset_at: Option<DateTime<Utc>>,
    pub last_checked: Option<DateTime<Utc>>,
    /// Consecutive limits observed without a provider-supplied reset time;
    /// drives exponential probe backoff.
    pub limit_strikes: i64,
}

#[derive(sqlx::FromRow)]
struct AgentRow {
    kind: String,
    install_status: String,
    version: Option<String>,
    availability: String,
    reset_at: Option<DateTime<Utc>>,
    last_checked: Option<DateTime<Utc>>,
    limit_strikes: i64,
}

impl TryFrom<AgentRow> for AgentRecord {
    type Error = DbError;

    fn try_from(row: AgentRow) -> Result<Self, Self::Error> {
        Ok(AgentRecord {
            kind: AgentKind::parse(&row.kind)
                .ok_or_else(|| DbError::InvalidEnum(row.kind.clone()))?,
            install_status: row.install_status,
            version: row.version,
            availability: AvailabilityState::parse(&row.availability)
                .ok_or_else(|| DbError::InvalidEnum(row.availability.clone()))?,
            reset_at: row.reset_at,
            last_checked: row.last_checked,
            limit_strikes: row.limit_strikes,
        })
    }
}

const SELECT: &str = "SELECT kind, install_status, version, availability, reset_at, \
    last_checked, limit_strikes FROM agents";

pub async fn get(pool: &SqlitePool, kind: AgentKind) -> Result<Option<AgentRecord>, DbError> {
    let row = sqlx::query_as::<_, AgentRow>(&format!("{SELECT} WHERE kind = ?"))
        .bind(kind.as_str())
        .fetch_optional(pool)
        .await?;
    row.map(AgentRecord::try_from).transpose()
}

pub async fn upsert(pool: &SqlitePool, record: &AgentRecord) -> Result<AgentRecord, DbError> {
    sqlx::query(
        "INSERT INTO agents (kind, install_status, version, availability, reset_at, last_checked, limit_strikes) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(kind) DO UPDATE SET install_status = excluded.install_status, \
         version = excluded.version, availability = excluded.availability, \
         reset_at = excluded.reset_at, last_checked = excluded.last_checked, \
         limit_strikes = excluded.limit_strikes",
    )
    .bind(record.kind.as_str())
    .bind(&record.install_status)
    .bind(&record.version)
    .bind(record.availability.as_str())
    .bind(record.reset_at)
    .bind(record.last_checked)
    .bind(record.limit_strikes)
    .execute(pool)
    .await?;

    get(pool, record.kind).await?.ok_or(DbError::NotFound)
}

pub async fn mark_limited(
    pool: &SqlitePool,
    kind: AgentKind,
    reset_at: Option<DateTime<Utc>>,
    limit_strikes: i64,
) -> Result<AgentRecord, DbError> {
    let existing = get(pool, kind).await?;
    let record = AgentRecord {
        kind,
        install_status: existing
            .as_ref()
            .map(|r| r.install_status.clone())
            .unwrap_or_else(|| "installed".to_string()),
        version: existing.and_then(|r| r.version),
        availability: AvailabilityState::Limited,
        reset_at,
        last_checked: Some(now()),
        limit_strikes,
    };
    upsert(pool, &record).await
}

pub async fn mark_available(pool: &SqlitePool, kind: AgentKind) -> Result<AgentRecord, DbError> {
    let existing = get(pool, kind).await?;
    let record = AgentRecord {
        kind,
        install_status: existing
            .as_ref()
            .map(|r| r.install_status.clone())
            .unwrap_or_else(|| "installed".to_string()),
        version: existing.and_then(|r| r.version),
        availability: AvailabilityState::Available,
        reset_at: None,
        last_checked: Some(now()),
        limit_strikes: 0,
    };
    upsert(pool, &record).await
}
