use am_proto::SessionEvent;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    #[sqlx(rename = "type")]
    kind: String,
    content_json: String,
    ts: DateTime<Utc>,
}

/// Persist a normalized session event to the transcript.
pub async fn insert(pool: &SqlitePool, ev: &SessionEvent) -> Result<(), DbError> {
    let content = serde_json::json!({ "text": ev.text, "data": ev.data });
    let content_json = serde_json::to_string(&content).unwrap_or_else(|_| "{}".into());
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, type, content_json, ts) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&ev.id)
    .bind(&ev.session_id)
    .bind(&ev.role)
    .bind(&ev.kind)
    .bind(&content_json)
    .bind(ev.ts)
    .execute(pool)
    .await?;
    Ok(())
}

/// The newest `limit` events of the given kinds for a session, oldest first.
/// Handoff summaries only need the tail of a transcript; loading the whole
/// thing scales with session length for no benefit.
pub async fn last_events_for_session(
    pool: &SqlitePool,
    session_id: &str,
    task_id: &str,
    kinds: &[&str],
    limit: i64,
) -> Result<Vec<SessionEvent>, DbError> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: sqlx::QueryBuilder<'_, sqlx::Sqlite> = sqlx::QueryBuilder::new(
        "SELECT id, session_id, role, type, content_json, ts FROM messages \
         WHERE session_id = ",
    );
    qb.push_bind(session_id);
    qb.push(" AND type IN (");
    let mut separated = qb.separated(", ");
    for kind in kinds {
        separated.push_bind(*kind);
    }
    separated.push_unseparated(") ORDER BY ts DESC, rowid DESC LIMIT ");
    qb.push_bind(limit);
    let rows: Vec<MessageRow> = qb.build_query_as().fetch_all(pool).await?;
    let mut events: Vec<SessionEvent> =
        rows.into_iter().map(|r| row_to_event(r, task_id)).collect();
    events.reverse();
    Ok(events)
}

/// Load a session's transcript. `task_id` is supplied by the caller (from the
/// owning session) since it is not duplicated on the message row.
pub async fn list_for_session(
    pool: &SqlitePool,
    session_id: &str,
    task_id: &str,
) -> Result<Vec<SessionEvent>, DbError> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, type, content_json, ts FROM messages \
         WHERE session_id = ? ORDER BY ts ASC, rowid ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| row_to_event(r, task_id)).collect())
}

fn row_to_event(r: MessageRow, task_id: &str) -> SessionEvent {
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
    SessionEvent {
        id: r.id,
        session_id: r.session_id,
        task_id: task_id.to_string(),
        role: r.role,
        kind: r.kind,
        text,
        data,
        ts: r.ts,
    }
}
