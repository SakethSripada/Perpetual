use am_proto::{new_id, now, AgentKind, QueuedTurn};
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct QueuedTurnRow {
    id: String,
    thread_id: String,
    agent_kind: String,
    permission: String,
    message: String,
    echo_user_message: i64,
    client_message_id: Option<String>,
    policy_envelope_id: Option<String>,
    created_at: DateTime<Utc>,
}

impl TryFrom<QueuedTurnRow> for QueuedTurn {
    type Error = DbError;
    fn try_from(r: QueuedTurnRow) -> Result<Self, DbError> {
        Ok(QueuedTurn {
            id: r.id,
            thread_id: r.thread_id,
            agent_kind: AgentKind::parse(&r.agent_kind)
                .ok_or_else(|| DbError::InvalidEnum(r.agent_kind.clone()))?,
            permission: r.permission,
            message: r.message,
            echo_user_message: r.echo_user_message != 0,
            client_message_id: r.client_message_id,
            policy_envelope_id: r.policy_envelope_id,
            created_at: r.created_at,
        })
    }
}

const SELECT: &str =
    "SELECT id, thread_id, agent_kind, permission, message, echo_user_message, client_message_id, policy_envelope_id, created_at FROM queued_turns";

pub async fn enqueue(
    pool: &SqlitePool,
    thread_id: &str,
    agent_kind: AgentKind,
    permission: &str,
    message: &str,
    policy_envelope_id: Option<&str>,
) -> Result<QueuedTurn, DbError> {
    enqueue_with_echo(
        pool,
        thread_id,
        agent_kind,
        permission,
        message,
        policy_envelope_id,
        true,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn enqueue_with_echo(
    pool: &SqlitePool,
    thread_id: &str,
    agent_kind: AgentKind,
    permission: &str,
    message: &str,
    policy_envelope_id: Option<&str>,
    echo_user_message: bool,
    client_message_id: Option<&str>,
) -> Result<QueuedTurn, DbError> {
    let turn = QueuedTurn {
        id: new_id(),
        thread_id: thread_id.to_string(),
        agent_kind,
        permission: permission.to_string(),
        message: message.to_string(),
        echo_user_message,
        client_message_id: client_message_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        policy_envelope_id: policy_envelope_id.map(str::to_string),
        created_at: now(),
    };
    sqlx::query(
        "INSERT INTO queued_turns (id, thread_id, agent_kind, permission, message, echo_user_message, client_message_id, policy_envelope_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&turn.id)
    .bind(&turn.thread_id)
    .bind(turn.agent_kind.as_str())
    .bind(&turn.permission)
    .bind(&turn.message)
    .bind(turn.echo_user_message)
    .bind(&turn.client_message_id)
    .bind(&turn.policy_envelope_id)
    .bind(turn.created_at)
    .execute(pool)
    .await?;
    Ok(turn)
}

pub async fn list_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Vec<QueuedTurn>, DbError> {
    let rows = sqlx::query_as::<_, QueuedTurnRow>(&format!(
        "{SELECT} WHERE thread_id = ? ORDER BY created_at ASC, id ASC"
    ))
    .bind(thread_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(QueuedTurn::try_from).collect()
}

pub async fn pop_next(pool: &SqlitePool, thread_id: &str) -> Result<Option<QueuedTurn>, DbError> {
    // Select-and-delete in one SQLite statement. A separate SELECT followed
    // by DELETE lets two scheduler/session completions claim the same queued
    // turn under concurrent wakeups.
    let row = sqlx::query_as::<_, QueuedTurnRow>(
        "DELETE FROM queued_turns WHERE id = (
           SELECT id FROM queued_turns
           WHERE thread_id = ?
           ORDER BY created_at ASC, id ASC
           LIMIT 1
         )
         RETURNING id, thread_id, agent_kind, permission, message,
                   echo_user_message, client_message_id, policy_envelope_id, created_at",
    )
    .bind(thread_id)
    .fetch_optional(pool)
    .await?;
    row.map(QueuedTurn::try_from).transpose()
}

pub async fn update_message(pool: &SqlitePool, id: &str, message: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE queued_turns SET message = ? WHERE id = ?")
        .bind(message)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Persist a new queue order. Queue position is derived from `created_at`
/// (see `list_for_thread` / `pop_next`), so we rewrite timestamps to match the
/// requested id order while preserving the ascending = next-to-run semantics.
pub async fn reorder(
    pool: &SqlitePool,
    thread_id: &str,
    ordered_ids: &[String],
) -> Result<(), DbError> {
    let base = now();
    let mut tx = pool.begin().await?;
    for (i, id) in ordered_ids.iter().enumerate() {
        let ts = base + Duration::milliseconds(i as i64);
        sqlx::query("UPDATE queued_turns SET created_at = ? WHERE id = ? AND thread_id = ?")
            .bind(ts)
            .bind(id)
            .bind(thread_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM queued_turns WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
