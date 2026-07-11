use am_proto::{
    new_id, now, AgentKind, AgentTurn, ComputeProviderKind, ExecutionBackend,
    LocalModelProviderKind, ModelTargetKind, SessionState,
};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct AgentTurnRow {
    id: String,
    thread_id: String,
    agent_kind: String,
    agent_session_id: Option<String>,
    status: String,
    permission: String,
    execution_backend: String,
    sandbox_name: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    local_provider: Option<String>,
    local_base_url: Option<String>,
    model_target: String,
    compute_lease_id: Option<String>,
    compute_provider: Option<String>,
    estimated_compute_cost_usd: Option<f64>,
    fallback_model_target: Option<String>,
    target_hash: Option<String>,
    policy_envelope_id: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

impl TryFrom<AgentTurnRow> for AgentTurn {
    type Error = DbError;
    fn try_from(r: AgentTurnRow) -> Result<Self, DbError> {
        Ok(AgentTurn {
            id: r.id,
            thread_id: r.thread_id,
            agent_kind: AgentKind::parse(&r.agent_kind)
                .ok_or_else(|| DbError::InvalidEnum(r.agent_kind.clone()))?,
            agent_session_id: r.agent_session_id,
            state: SessionState::parse(&r.status)
                .ok_or_else(|| DbError::InvalidEnum(r.status.clone()))?,
            permission: r.permission,
            execution_backend: ExecutionBackend::parse(&r.execution_backend)
                .ok_or_else(|| DbError::InvalidEnum(r.execution_backend.clone()))?,
            sandbox_name: r.sandbox_name,
            model: r.model,
            reasoning: r.reasoning,
            local_provider: parse_local_provider(r.local_provider)?,
            local_base_url: r.local_base_url,
            model_target: ModelTargetKind::parse(&r.model_target)
                .ok_or_else(|| DbError::InvalidEnum(r.model_target.clone()))?,
            compute_lease_id: r.compute_lease_id,
            compute_provider: parse_compute_provider(r.compute_provider)?,
            estimated_compute_cost_usd: r.estimated_compute_cost_usd,
            fallback_model_target: parse_model_target(r.fallback_model_target)?,
            target_hash: r.target_hash,
            policy_envelope_id: r.policy_envelope_id,
            started_at: r.started_at,
            ended_at: r.ended_at,
        })
    }
}

const SELECT: &str = "SELECT id, thread_id, agent_kind, agent_session_id, status, \
    permission, execution_backend, sandbox_name, model, reasoning, local_provider, local_base_url, \
    model_target, compute_lease_id, compute_provider, estimated_compute_cost_usd, \
    fallback_model_target, target_hash, policy_envelope_id, started_at, ended_at FROM agent_turns";

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &SqlitePool,
    thread_id: &str,
    agent_kind: AgentKind,
    permission: &str,
    execution_backend: ExecutionBackend,
    sandbox_name: Option<&str>,
    model: Option<&str>,
    reasoning: Option<&str>,
    local_provider: Option<LocalModelProviderKind>,
    local_base_url: Option<&str>,
    model_target: ModelTargetKind,
    compute_lease_id: Option<&str>,
    compute_provider: Option<ComputeProviderKind>,
    estimated_compute_cost_usd: Option<f64>,
    fallback_model_target: Option<ModelTargetKind>,
    target_hash: Option<&str>,
    policy_envelope_id: Option<&str>,
) -> Result<AgentTurn, DbError> {
    let turn = AgentTurn {
        id: new_id(),
        thread_id: thread_id.to_string(),
        agent_kind,
        agent_session_id: None,
        state: SessionState::Running,
        permission: permission.to_string(),
        execution_backend,
        sandbox_name: sandbox_name.map(|name| name.to_string()),
        model: model.map(|value| value.to_string()),
        reasoning: reasoning.map(|value| value.to_string()),
        local_provider,
        local_base_url: local_base_url.map(|value| value.to_string()),
        model_target,
        compute_lease_id: compute_lease_id.map(|value| value.to_string()),
        compute_provider,
        estimated_compute_cost_usd,
        fallback_model_target,
        target_hash: target_hash.map(|value| value.to_string()),
        policy_envelope_id: policy_envelope_id.map(|value| value.to_string()),
        started_at: now(),
        ended_at: None,
    };
    sqlx::query(
        "INSERT INTO agent_turns (id, thread_id, agent_kind, agent_session_id, status, \
         permission, execution_backend, sandbox_name, model, reasoning, local_provider, \
         local_base_url, model_target, compute_lease_id, compute_provider, \
         estimated_compute_cost_usd, fallback_model_target, target_hash, started_at, ended_at, \
         policy_envelope_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)",
    )
    .bind(&turn.id)
    .bind(&turn.thread_id)
    .bind(turn.agent_kind.as_str())
    .bind(&turn.agent_session_id)
    .bind(turn.state.as_str())
    .bind(&turn.permission)
    .bind(turn.execution_backend.as_str())
    .bind(&turn.sandbox_name)
    .bind(&turn.model)
    .bind(&turn.reasoning)
    .bind(turn.local_provider.map(|provider| provider.as_str()))
    .bind(&turn.local_base_url)
    .bind(turn.model_target.as_str())
    .bind(&turn.compute_lease_id)
    .bind(turn.compute_provider.map(|provider| provider.as_str()))
    .bind(turn.estimated_compute_cost_usd)
    .bind(turn.fallback_model_target.map(|target| target.as_str()))
    .bind(&turn.target_hash)
    .bind(turn.started_at)
    .bind(&turn.policy_envelope_id)
    .execute(pool)
    .await?;
    Ok(turn)
}

fn parse_local_provider(value: Option<String>) -> Result<Option<LocalModelProviderKind>, DbError> {
    value
        .map(|s| LocalModelProviderKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
        .transpose()
}

fn parse_compute_provider(value: Option<String>) -> Result<Option<ComputeProviderKind>, DbError> {
    value
        .map(|s| ComputeProviderKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
        .transpose()
}

fn parse_model_target(value: Option<String>) -> Result<Option<ModelTargetKind>, DbError> {
    value
        .map(|s| ModelTargetKind::parse(&s).ok_or_else(|| DbError::InvalidEnum(s.clone())))
        .transpose()
}

pub async fn set_agent_session_id(
    pool: &SqlitePool,
    id: &str,
    agent_session_id: &str,
) -> Result<(), DbError> {
    sqlx::query("UPDATE agent_turns SET agent_session_id = ? WHERE id = ?")
        .bind(agent_session_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn finish(pool: &SqlitePool, id: &str, state: SessionState) -> Result<(), DbError> {
    sqlx::query("UPDATE agent_turns SET status = ?, ended_at = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_for_thread(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Vec<AgentTurn>, DbError> {
    let rows = sqlx::query_as::<_, AgentTurnRow>(&format!(
        "{SELECT} WHERE thread_id = ? ORDER BY started_at ASC"
    ))
    .bind(thread_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(AgentTurn::try_from).collect()
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<AgentTurn>, DbError> {
    let row = sqlx::query_as::<_, AgentTurnRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(AgentTurn::try_from).transpose()
}

pub async fn mark_orphans_interrupted(pool: &SqlitePool) -> Result<u64, DbError> {
    let res =
        sqlx::query("UPDATE agent_turns SET status = ?, ended_at = ? WHERE status = 'running'")
            .bind(SessionState::Interrupted.as_str())
            .bind(now())
            .execute(pool)
            .await?;
    Ok(res.rows_affected())
}
