use sqlx::SqlitePool;

use crate::DbError;

/// Read a key/value setting, if present.
pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>, DbError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.0))
}

/// Upsert a key/value setting.
pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}
