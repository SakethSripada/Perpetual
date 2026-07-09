use am_proto::{new_id, now, ActivityEvent, NewActivity};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    project_id: Option<String>,
    task_id: Option<String>,
    kind: String,
    payload_json: String,
    ts: DateTime<Utc>,
}

impl TryFrom<EventRow> for ActivityEvent {
    type Error = DbError;
    fn try_from(r: EventRow) -> Result<Self, DbError> {
        let payload = serde_json::from_str(&r.payload_json).unwrap_or(serde_json::Value::Null);
        Ok(ActivityEvent {
            id: r.id,
            project_id: r.project_id,
            task_id: r.task_id,
            kind: r.kind,
            payload,
            ts: r.ts,
        })
    }
}

const SELECT: &str = "SELECT id, project_id, task_id, kind, payload_json, ts FROM events";

pub async fn record(pool: &SqlitePool, input: NewActivity) -> Result<ActivityEvent, DbError> {
    let event = ActivityEvent {
        id: new_id(),
        project_id: input.project_id,
        task_id: input.task_id,
        kind: input.kind,
        payload: input.payload,
        ts: now(),
    };
    let payload_json = serde_json::to_string(&event.payload).unwrap_or_else(|_| "null".into());

    sqlx::query(
        "INSERT INTO events (id, project_id, task_id, kind, payload_json, ts) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(&event.project_id)
    .bind(&event.task_id)
    .bind(&event.kind)
    .bind(&payload_json)
    .bind(event.ts)
    .execute(pool)
    .await?;

    Ok(event)
}

pub async fn list_for_project(
    pool: &SqlitePool,
    project_id: &str,
    limit: i64,
) -> Result<Vec<ActivityEvent>, DbError> {
    let rows = sqlx::query_as::<_, EventRow>(&format!(
        "{SELECT} WHERE project_id = ? ORDER BY ts DESC LIMIT ?"
    ))
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(ActivityEvent::try_from).collect()
}

pub async fn list_recent(pool: &SqlitePool, limit: i64) -> Result<Vec<ActivityEvent>, DbError> {
    let rows = sqlx::query_as::<_, EventRow>(&format!("{SELECT} ORDER BY ts DESC LIMIT ?"))
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(ActivityEvent::try_from).collect()
}
