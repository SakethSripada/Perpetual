use crate::DbError;
use am_proto::{now, AvailabilityState};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct ProviderAccountRecord {
    pub account_id: String,
    pub availability: AvailabilityState,
    pub reset_at: Option<DateTime<Utc>>,
    pub limit_strikes: i64,
    pub last_checked: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct Row {
    account_id: String,
    availability: String,
    reset_at: Option<DateTime<Utc>>,
    limit_strikes: i64,
    last_checked: Option<DateTime<Utc>>,
}

impl TryFrom<Row> for ProviderAccountRecord {
    type Error = DbError;
    fn try_from(row: Row) -> Result<Self, Self::Error> {
        Ok(Self {
            account_id: row.account_id,
            availability: AvailabilityState::parse(&row.availability)
                .ok_or_else(|| DbError::InvalidEnum(row.availability))?,
            reset_at: row.reset_at,
            limit_strikes: row.limit_strikes,
            last_checked: row.last_checked,
        })
    }
}

const SELECT: &str = "SELECT account_id, availability, reset_at, limit_strikes, last_checked FROM provider_account_states";
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<ProviderAccountRecord>, DbError> {
    sqlx::query_as::<_, Row>(&format!("{SELECT} WHERE account_id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(TryInto::try_into)
        .transpose()
}
pub async fn list(pool: &SqlitePool) -> Result<Vec<ProviderAccountRecord>, DbError> {
    sqlx::query_as::<_, Row>(SELECT)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
}
pub async fn mark_limited(
    pool: &SqlitePool,
    id: &str,
    reset_at: Option<DateTime<Utc>>,
    strikes: i64,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO provider_account_states (account_id, availability, reset_at, limit_strikes, last_checked) VALUES (?, 'limited', ?, ?, ?) ON CONFLICT(account_id) DO UPDATE SET availability='limited', reset_at=excluded.reset_at, limit_strikes=excluded.limit_strikes, last_checked=excluded.last_checked")
        .bind(id).bind(reset_at).bind(strikes).bind(now()).execute(pool).await?;
    Ok(())
}
pub async fn mark_available(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("INSERT INTO provider_account_states (account_id, availability, reset_at, limit_strikes, last_checked) VALUES (?, 'available', NULL, 0, ?) ON CONFLICT(account_id) DO UPDATE SET availability='available', reset_at=NULL, limit_strikes=0, last_checked=excluded.last_checked")
        .bind(id).bind(now()).execute(pool).await?;
    Ok(())
}
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM provider_account_states WHERE account_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
