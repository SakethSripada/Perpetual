use am_proto::{new_id, now, AgentKind};
use sqlx::SqlitePool;

use crate::DbError;

/// Persist provider-reported token deltas. The caller reconciles cumulative
/// provider notifications before writing, so this table remains additive and
/// useful for session totals after a process restart.
#[allow(clippy::too_many_arguments)]
pub async fn record_tokens(
    pool: &SqlitePool,
    project_id: Option<&str>,
    session_id: Option<&str>,
    run_id: Option<&str>,
    agent: AgentKind,
    model: Option<&str>,
    policy_envelope_id: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO usage_ledger \
         (id, ts, project_id, session_id, run_id, agent_kind, provider, model, input_tokens, output_tokens, policy_envelope_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(now())
    .bind(project_id)
    .bind(session_id)
    .bind(run_id)
    .bind(agent.as_str())
    .bind(agent.as_str())
    .bind(model)
    .bind(input_tokens.min(i64::MAX as u64) as i64)
    .bind(output_tokens.min(i64::MAX as u64) as i64)
    .bind(policy_envelope_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn total_for_session(pool: &SqlitePool, session_id: &str) -> Result<u64, DbError> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0) \
         FROM usage_ledger WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0.max(0) as u64)
}
