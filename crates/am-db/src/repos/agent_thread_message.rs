use am_proto::AgentThreadEvent;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    thread_id: String,
    turn_id: String,
    role: String,
    #[sqlx(rename = "type")]
    kind: String,
    content_json: String,
    ts: DateTime<Utc>,
}

pub async fn insert(pool: &SqlitePool, ev: &AgentThreadEvent) -> Result<(), DbError> {
    let content = serde_json::json!({ "text": ev.text, "data": ev.data });
    let content_json = serde_json::to_string(&content).unwrap_or_else(|_| "{}".into());
    sqlx::query(
        "INSERT INTO agent_thread_messages (id, thread_id, turn_id, role, type, content_json, ts) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&ev.id)
    .bind(&ev.thread_id)
    .bind(&ev.turn_id)
    .bind(&ev.role)
    .bind(&ev.kind)
    .bind(&content_json)
    .bind(ev.ts)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Vec<AgentThreadEvent>, DbError> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, thread_id, turn_id, role, type, content_json, ts \
         FROM agent_thread_messages WHERE thread_id = ? ORDER BY ts ASC, rowid ASC",
    )
    .bind(thread_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_event).collect())
}

pub async fn list_for_turn(
    pool: &SqlitePool,
    turn_id: &str,
) -> Result<Vec<AgentThreadEvent>, DbError> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, thread_id, turn_id, role, type, content_json, ts \
         FROM agent_thread_messages WHERE turn_id = ? ORDER BY ts ASC, rowid ASC",
    )
    .bind(turn_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_event).collect())
}

fn row_to_event(r: MessageRow) -> AgentThreadEvent {
    let content: serde_json::Value =
        serde_json::from_str(&r.content_json).unwrap_or(serde_json::Value::Null);
    let text = content
        .get("text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let data = content
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    AgentThreadEvent {
        id: r.id,
        thread_id: r.thread_id,
        turn_id: r.turn_id,
        role: r.role,
        kind: r.kind,
        text,
        data,
        ts: r.ts,
    }
}
