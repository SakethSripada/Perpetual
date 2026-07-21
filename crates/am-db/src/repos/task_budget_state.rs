use am_proto::now;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::DbError;

/// Read private per-thread budget enforcement state. The state is deliberately
/// kept outside `AgentThread` so it cannot leak through normal wire snapshots.
pub async fn get(pool: &SqlitePool, thread_id: &str) -> Result<Value, DbError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT state_json FROM agent_thread_budget_state WHERE thread_id = ?")
            .bind(thread_id)
            .fetch_optional(pool)
            .await?;
    row.map(|(state_json,)| {
        serde_json::from_str(&state_json).map_err(|err| DbError::Serde(err.to_string()))
    })
    .transpose()
    .map(|state| state.unwrap_or_else(|| serde_json::json!({})))
}

pub async fn save(pool: &SqlitePool, thread_id: &str, state: &Value) -> Result<(), DbError> {
    let state_json = serde_json::to_string(state).map_err(|err| DbError::Serde(err.to_string()))?;
    sqlx::query(
        "INSERT INTO agent_thread_budget_state (thread_id, state_json, updated_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(thread_id) DO UPDATE SET state_json = excluded.state_json, updated_at = excluded.updated_at",
    )
    .bind(thread_id)
    .bind(state_json)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}
