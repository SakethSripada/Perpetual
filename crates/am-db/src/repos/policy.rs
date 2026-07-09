use am_proto::{
    new_id, now, AgentKind, ApiGatewayConfig, BudgetPolicy, BudgetPolicyRecord, BudgetStatus,
    PolicyApprovalGrant, PolicyAuditExport, PolicyBinding, PolicyDecision, PolicyDocument,
    PolicyScope, PolicyScopeKind, SessionPolicyEnvelope, UsageLedgerEntry,
};
use chrono::{DateTime, Utc};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::DbError;

#[derive(sqlx::FromRow)]
struct PolicyDocumentRow {
    id: String,
    name: String,
    description: Option<String>,
    enabled: bool,
    priority: i64,
    rules_json: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<PolicyDocumentRow> for PolicyDocument {
    type Error = DbError;

    fn try_from(row: PolicyDocumentRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            description: row.description,
            enabled: row.enabled,
            priority: row.priority,
            rules: serde_json::from_str(&row.rules_json).unwrap_or_default(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PolicyBindingRow {
    id: String,
    document_id: String,
    scope_kind: String,
    scope_id: Option<String>,
    created_at: DateTime<Utc>,
}

impl TryFrom<PolicyBindingRow> for PolicyBinding {
    type Error = DbError;

    fn try_from(row: PolicyBindingRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            document_id: row.document_id,
            scope: PolicyScope {
                kind: parse_scope_kind(&row.scope_kind)?,
                id: row.scope_id,
            },
            created_at: row.created_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct EnvelopeRow {
    envelope_json: String,
}

#[derive(sqlx::FromRow)]
struct EvaluationRow {
    decision_json: String,
}

#[derive(sqlx::FromRow)]
struct UsageRow {
    id: String,
    ts: DateTime<Utc>,
    org_id: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
    project_id: Option<String>,
    group_id: Option<String>,
    repo_id: Option<String>,
    session_id: Option<String>,
    run_id: Option<String>,
    agent_kind: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    traffic_kind: Option<String>,
    api_source: Option<String>,
    source_label: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    estimated_cost_usd: Option<f64>,
    policy_envelope_id: Option<String>,
    request_count: i64,
    status_code: Option<i64>,
}

impl TryFrom<UsageRow> for UsageLedgerEntry {
    type Error = DbError;

    fn try_from(row: UsageRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            ts: row.ts,
            org_id: row.org_id,
            team_id: row.team_id,
            user_id: row.user_id,
            project_id: row.project_id,
            group_id: row.group_id,
            repo_id: row.repo_id,
            session_id: row.session_id,
            run_id: row.run_id,
            agent: row
                .agent_kind
                .map(|agent| {
                    AgentKind::parse(&agent).ok_or_else(|| DbError::InvalidEnum(agent.clone()))
                })
                .transpose()?,
            provider: row.provider,
            model: row.model,
            traffic_kind: row.traffic_kind,
            api_source: row.api_source,
            source_label: row.source_label,
            input_tokens: row.input_tokens.max(0) as u64,
            output_tokens: row.output_tokens.max(0) as u64,
            estimated_cost_usd: row.estimated_cost_usd,
            policy_envelope_id: row.policy_envelope_id,
            request_count: row.request_count.max(0) as u64,
            status_code: row.status_code.map(|status| status.max(0) as u16),
        })
    }
}

#[derive(sqlx::FromRow)]
struct BudgetPolicyRow {
    id: String,
    name: String,
    enabled: bool,
    scope_kind: String,
    scope_id: Option<String>,
    provider: Option<String>,
    agent_kind: Option<String>,
    model: Option<String>,
    traffic_kind: Option<String>,
    enforce_managed_sessions: bool,
    enforce_api_gateway: bool,
    soft_token_cap: Option<i64>,
    hard_token_cap: Option<i64>,
    soft_cost_cap_usd: Option<f64>,
    hard_cost_cap_usd: Option<f64>,
    warning_threshold: Option<f64>,
    window: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<BudgetPolicyRow> for BudgetPolicyRecord {
    type Error = DbError;

    fn try_from(row: BudgetPolicyRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            enabled: row.enabled,
            scope: PolicyScope {
                kind: parse_scope_kind(&row.scope_kind)?,
                id: row.scope_id,
            },
            provider: row.provider,
            agent: row
                .agent_kind
                .map(|agent| {
                    AgentKind::parse(&agent).ok_or_else(|| DbError::InvalidEnum(agent.clone()))
                })
                .transpose()?,
            model: row.model,
            traffic_kind: row.traffic_kind,
            enforce_managed_sessions: row.enforce_managed_sessions,
            enforce_api_gateway: row.enforce_api_gateway,
            budget: BudgetPolicy {
                soft_token_cap: row.soft_token_cap.map(|value| value.max(0) as u64),
                hard_token_cap: row.hard_token_cap.map(|value| value.max(0) as u64),
                soft_cost_cap_usd: row.soft_cost_cap_usd,
                hard_cost_cap_usd: row.hard_cost_cap_usd,
                warning_threshold: row.warning_threshold,
                window: row.window,
            },
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ApiGatewayConfigRow {
    id: String,
    provider: String,
    name: String,
    enabled: bool,
    enforce_policies: bool,
    listen_host: String,
    listen_port: Option<i64>,
    upstream_base_url: String,
    auth_env_var: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<ApiGatewayConfigRow> for ApiGatewayConfig {
    fn from(row: ApiGatewayConfigRow) -> Self {
        Self {
            id: row.id,
            provider: row.provider,
            name: row.name,
            enabled: row.enabled,
            enforce_policies: row.enforce_policies,
            listen_host: row.listen_host,
            listen_port: row
                .listen_port
                .map(|port| port.clamp(0, u16::MAX as i64) as u16),
            upstream_base_url: row.upstream_base_url,
            auth_env_var: row.auth_env_var,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct ApprovalGrantRow {
    id: String,
    request_hash: String,
    status: String,
    reason: Option<String>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
}

impl From<ApprovalGrantRow> for PolicyApprovalGrant {
    fn from(row: ApprovalGrantRow) -> Self {
        Self {
            id: row.id,
            request_hash: row.request_hash,
            status: row.status,
            reason: row.reason,
            created_at: row.created_at,
            resolved_at: row.resolved_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct AuditExportRow {
    id: String,
    created_at: DateTime<Utc>,
    format: String,
    body: String,
}

impl From<AuditExportRow> for PolicyAuditExport {
    fn from(row: AuditExportRow) -> Self {
        Self {
            id: row.id,
            created_at: row.created_at,
            format: row.format,
            body: row.body,
        }
    }
}

pub async fn list_documents(pool: &SqlitePool) -> Result<Vec<PolicyDocument>, DbError> {
    let rows = sqlx::query_as::<_, PolicyDocumentRow>(
        "SELECT id, name, description, enabled, priority, rules_json, created_at, updated_at \
         FROM policy_documents ORDER BY priority ASC, name ASC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(PolicyDocument::try_from).collect()
}

pub async fn upsert_document(
    pool: &SqlitePool,
    mut document: PolicyDocument,
) -> Result<PolicyDocument, DbError> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM policy_documents WHERE id = ?")
        .bind(&document.id)
        .fetch_optional(pool)
        .await?;
    let ts = now();
    if exists.is_some() {
        document.updated_at = ts;
    } else {
        if document.id.trim().is_empty() {
            document.id = new_id();
        }
        document.created_at = ts;
        document.updated_at = ts;
    }
    let rules_json = serde_json::to_string(&document.rules).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO policy_documents (id, name, description, enabled, priority, rules_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, description = excluded.description, \
         enabled = excluded.enabled, priority = excluded.priority, rules_json = excluded.rules_json, \
         updated_at = excluded.updated_at",
    )
    .bind(&document.id)
    .bind(&document.name)
    .bind(&document.description)
    .bind(document.enabled)
    .bind(document.priority)
    .bind(rules_json)
    .bind(document.created_at)
    .bind(document.updated_at)
    .execute(pool)
    .await?;
    Ok(document)
}

pub async fn delete_document(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM policy_documents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_bindings(pool: &SqlitePool) -> Result<Vec<PolicyBinding>, DbError> {
    let rows = sqlx::query_as::<_, PolicyBindingRow>(
        "SELECT id, document_id, scope_kind, scope_id, created_at FROM policy_bindings \
         ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(PolicyBinding::try_from).collect()
}

pub async fn upsert_budget_policy(
    pool: &SqlitePool,
    mut policy: BudgetPolicyRecord,
) -> Result<BudgetPolicyRecord, DbError> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM budget_policies WHERE id = ?")
        .bind(&policy.id)
        .fetch_optional(pool)
        .await?;
    let ts = now();
    if exists.is_some() {
        policy.updated_at = ts;
    } else {
        if policy.id.trim().is_empty() {
            policy.id = new_id();
        }
        policy.created_at = ts;
        policy.updated_at = ts;
    }
    sqlx::query(
        "INSERT INTO budget_policies \
         (id, name, enabled, scope_kind, scope_id, provider, agent_kind, model, traffic_kind, \
          enforce_managed_sessions, enforce_api_gateway, soft_token_cap, hard_token_cap, \
          soft_cost_cap_usd, hard_cost_cap_usd, warning_threshold, window, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled, \
          scope_kind = excluded.scope_kind, scope_id = excluded.scope_id, provider = excluded.provider, \
          agent_kind = excluded.agent_kind, model = excluded.model, traffic_kind = excluded.traffic_kind, \
          enforce_managed_sessions = excluded.enforce_managed_sessions, \
          enforce_api_gateway = excluded.enforce_api_gateway, soft_token_cap = excluded.soft_token_cap, \
          hard_token_cap = excluded.hard_token_cap, soft_cost_cap_usd = excluded.soft_cost_cap_usd, \
          hard_cost_cap_usd = excluded.hard_cost_cap_usd, warning_threshold = excluded.warning_threshold, \
          window = excluded.window, updated_at = excluded.updated_at",
    )
    .bind(&policy.id)
    .bind(&policy.name)
    .bind(policy.enabled)
    .bind(policy.scope.kind.as_str())
    .bind(&policy.scope.id)
    .bind(&policy.provider)
    .bind(policy.agent.map(|agent| agent.as_str()))
    .bind(&policy.model)
    .bind(&policy.traffic_kind)
    .bind(policy.enforce_managed_sessions)
    .bind(policy.enforce_api_gateway)
    .bind(policy.budget.soft_token_cap.map(|v| v as i64))
    .bind(policy.budget.hard_token_cap.map(|v| v as i64))
    .bind(policy.budget.soft_cost_cap_usd)
    .bind(policy.budget.hard_cost_cap_usd)
    .bind(policy.budget.warning_threshold)
    .bind(&policy.budget.window)
    .bind(policy.created_at)
    .bind(policy.updated_at)
    .execute(pool)
    .await?;
    Ok(policy)
}

pub async fn delete_budget_policy(pool: &SqlitePool, id: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM budget_policies WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_budget_policies(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<BudgetPolicyRecord>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, BudgetPolicyRow>(
                &format!("{BUDGET_SELECT} WHERE scope_kind != 'project' OR scope_id = ? ORDER BY scope_kind ASC, name ASC"),
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, BudgetPolicyRow>(&format!(
                "{BUDGET_SELECT} ORDER BY scope_kind ASC, name ASC"
            ))
            .fetch_all(pool)
            .await?
        }
    };
    rows.into_iter().map(BudgetPolicyRecord::try_from).collect()
}

pub async fn upsert_api_gateway_config(
    pool: &SqlitePool,
    mut config: ApiGatewayConfig,
) -> Result<ApiGatewayConfig, DbError> {
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM api_gateway_configs WHERE id = ?")
            .bind(&config.id)
            .fetch_optional(pool)
            .await?;
    let ts = now();
    if exists.is_some() {
        config.updated_at = ts;
    } else {
        if config.id.trim().is_empty() {
            config.id = new_id();
        }
        config.created_at = ts;
        config.updated_at = ts;
    }
    sqlx::query(
        "INSERT INTO api_gateway_configs \
         (id, provider, name, enabled, enforce_policies, listen_host, listen_port, upstream_base_url, \
          auth_env_var, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET provider = excluded.provider, name = excluded.name, \
          enabled = excluded.enabled, enforce_policies = excluded.enforce_policies, \
          listen_host = excluded.listen_host, listen_port = excluded.listen_port, \
          upstream_base_url = excluded.upstream_base_url, auth_env_var = excluded.auth_env_var, \
          updated_at = excluded.updated_at",
    )
    .bind(&config.id)
    .bind(&config.provider)
    .bind(&config.name)
    .bind(config.enabled)
    .bind(config.enforce_policies)
    .bind(&config.listen_host)
    .bind(config.listen_port.map(|port| port as i64))
    .bind(&config.upstream_base_url)
    .bind(&config.auth_env_var)
    .bind(config.created_at)
    .bind(config.updated_at)
    .execute(pool)
    .await?;
    Ok(config)
}

pub async fn list_api_gateway_configs(pool: &SqlitePool) -> Result<Vec<ApiGatewayConfig>, DbError> {
    let rows = sqlx::query_as::<_, ApiGatewayConfigRow>(
        "SELECT id, provider, name, enabled, enforce_policies, listen_host, listen_port, \
         upstream_base_url, auth_env_var, created_at, updated_at FROM api_gateway_configs \
         ORDER BY provider ASC, name ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(ApiGatewayConfig::from).collect())
}

pub async fn insert_evaluation(
    pool: &SqlitePool,
    decision: &PolicyDecision,
) -> Result<(), DbError> {
    let request_json = serde_json::to_string(&decision.request).unwrap_or_default();
    let decision_json = serde_json::to_string(decision).unwrap_or_default();
    sqlx::query(
        "INSERT INTO policy_evaluations (id, request_id, envelope_id, request_json, decision_json, \
         action, project_id, session_id, run_id, group_id, provider, traffic_kind, api_source, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&decision.id)
    .bind(&decision.request_id)
    .bind(&decision.envelope_id)
    .bind(request_json)
    .bind(decision_json)
    .bind(decision.action.as_str())
    .bind(&decision.request.project_id)
    .bind(&decision.request.session_id)
    .bind(&decision.request.run_id)
    .bind(&decision.request.group_id)
    .bind(&decision.request.provider)
    .bind(&decision.request.traffic_kind)
    .bind(&decision.request.api_source)
    .bind(decision.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_evaluations(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> Result<Vec<PolicyDecision>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, EvaluationRow>(
                "SELECT decision_json FROM policy_evaluations WHERE project_id = ? \
                 ORDER BY created_at DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, EvaluationRow>(
                "SELECT decision_json FROM policy_evaluations ORDER BY created_at DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<PolicyDecision>(&row.decision_json).ok())
        .collect())
}

pub async fn insert_envelope(
    pool: &SqlitePool,
    envelope: &SessionPolicyEnvelope,
) -> Result<(), DbError> {
    let envelope_json = serde_json::to_string(envelope).unwrap_or_default();
    sqlx::query(
        "INSERT INTO policy_envelopes (id, request_id, decision_id, envelope_json, project_id, \
         session_id, run_id, agent_kind, runtime, action, group_id, provider, traffic_kind, api_source, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&envelope.id)
    .bind(&envelope.request_id)
    .bind(&envelope.decision_id)
    .bind(envelope_json)
    .bind(&envelope.project_id)
    .bind(&envelope.session_id)
    .bind(&envelope.run_id)
    .bind(envelope.agent.as_str())
    .bind(envelope.runtime.as_str())
    .bind(envelope.action.as_str())
    .bind(&envelope.group_id)
    .bind(&envelope.provider)
    .bind(&envelope.traffic_kind)
    .bind(&envelope.api_source)
    .bind(envelope.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_envelope(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<SessionPolicyEnvelope>, DbError> {
    let row =
        sqlx::query_as::<_, EnvelopeRow>("SELECT envelope_json FROM policy_envelopes WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|row| serde_json::from_str(&row.envelope_json).ok()))
}

pub async fn list_envelopes(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> Result<Vec<SessionPolicyEnvelope>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, EnvelopeRow>(
                "SELECT envelope_json FROM policy_envelopes WHERE project_id = ? \
                 ORDER BY created_at DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, EnvelopeRow>(
                "SELECT envelope_json FROM policy_envelopes ORDER BY created_at DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<SessionPolicyEnvelope>(&row.envelope_json).ok())
        .collect())
}

pub async fn insert_usage(pool: &SqlitePool, entry: &UsageLedgerEntry) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO usage_ledger (id, ts, org_id, team_id, user_id, project_id, repo_id, session_id, \
         run_id, agent_kind, provider, model, input_tokens, output_tokens, estimated_cost_usd, \
         policy_envelope_id, group_id, traffic_kind, api_source, source_label, request_count, status_code) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(entry.ts)
    .bind(&entry.org_id)
    .bind(&entry.team_id)
    .bind(&entry.user_id)
    .bind(&entry.project_id)
    .bind(&entry.repo_id)
    .bind(&entry.session_id)
    .bind(&entry.run_id)
    .bind(entry.agent.map(|agent| agent.as_str()))
    .bind(&entry.provider)
    .bind(&entry.model)
    .bind(entry.input_tokens as i64)
    .bind(entry.output_tokens as i64)
    .bind(entry.estimated_cost_usd)
    .bind(&entry.policy_envelope_id)
    .bind(&entry.group_id)
    .bind(&entry.traffic_kind)
    .bind(&entry.api_source)
    .bind(&entry.source_label)
    .bind(entry.request_count as i64)
    .bind(entry.status_code.map(|status| status as i64))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_usage(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> Result<Vec<UsageLedgerEntry>, DbError> {
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageRow>(&format!(
                "{USAGE_SELECT} WHERE project_id = ? ORDER BY ts DESC LIMIT ?"
            ))
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, UsageRow>(&format!("{USAGE_SELECT} ORDER BY ts DESC LIMIT ?"))
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
    };
    rows.into_iter().map(UsageLedgerEntry::try_from).collect()
}

pub async fn budget_status(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<BudgetStatus>, DbError> {
    let policies = list_budget_policies(pool, project_id).await?;
    if policies.is_empty() {
        return default_budget_status(pool, project_id).await;
    }

    let mut statuses = Vec::new();
    for policy in policies.into_iter().filter(|policy| policy.enabled) {
        let (input, output, cost) = usage_totals_for_budget(pool, &policy, project_id).await?;
        let input_tokens = input.unwrap_or_default().max(0) as u64;
        let output_tokens = output.unwrap_or_default().max(0) as u64;
        let total_tokens = input_tokens + output_tokens;
        let estimated_cost_usd = cost;
        let hard_exceeded = policy
            .budget
            .hard_token_cap
            .is_some_and(|cap| total_tokens >= cap)
            || policy
                .budget
                .hard_cost_cap_usd
                .zip(estimated_cost_usd)
                .is_some_and(|(cap, used)| used >= cap);
        let warning = budget_warning(&policy, total_tokens, estimated_cost_usd);
        statuses.push(BudgetStatus {
            scope: policy.scope.kind.as_str().into(),
            subject_id: policy.scope.id.clone(),
            provider: policy.provider.clone(),
            agent: policy.agent,
            model: policy.model.clone(),
            traffic_kind: policy.traffic_kind.clone(),
            enforce_managed_sessions: policy.enforce_managed_sessions,
            enforce_api_gateway: policy.enforce_api_gateway,
            input_tokens,
            output_tokens,
            total_tokens,
            estimated_cost_usd,
            soft_token_cap: policy.budget.soft_token_cap,
            hard_token_cap: policy.budget.hard_token_cap,
            warning,
            hard_exceeded,
        });
    }
    Ok(statuses)
}

pub async fn approved_grant_for_hash(
    pool: &SqlitePool,
    request_hash: &str,
) -> Result<Option<PolicyApprovalGrant>, DbError> {
    let row = sqlx::query_as::<_, ApprovalGrantRow>(
        "SELECT id, request_hash, status, reason, created_at, resolved_at FROM policy_approval_grants \
         WHERE request_hash = ? AND status = 'approved' ORDER BY resolved_at DESC LIMIT 1",
    )
    .bind(request_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(PolicyApprovalGrant::from))
}

pub async fn create_pending_grant(
    pool: &SqlitePool,
    request_hash: &str,
    reason: Option<&str>,
) -> Result<PolicyApprovalGrant, DbError> {
    if let Some(existing) = sqlx::query_as::<_, ApprovalGrantRow>(
        "SELECT id, request_hash, status, reason, created_at, resolved_at FROM policy_approval_grants \
         WHERE request_hash = ? AND status = 'pending' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(request_hash)
    .fetch_optional(pool)
    .await?
    {
        return Ok(existing.into());
    }
    let grant = PolicyApprovalGrant {
        id: new_id(),
        request_hash: request_hash.to_string(),
        status: "pending".into(),
        reason: reason.map(str::to_string),
        created_at: now(),
        resolved_at: None,
    };
    sqlx::query(
        "INSERT INTO policy_approval_grants (id, request_hash, status, reason, created_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, NULL)",
    )
    .bind(&grant.id)
    .bind(&grant.request_hash)
    .bind(&grant.status)
    .bind(&grant.reason)
    .bind(grant.created_at)
    .execute(pool)
    .await?;
    Ok(grant)
}

pub async fn list_approval_grants(
    pool: &SqlitePool,
    status: Option<&str>,
) -> Result<Vec<PolicyApprovalGrant>, DbError> {
    let rows = match status {
        Some(status) => {
            sqlx::query_as::<_, ApprovalGrantRow>(
                "SELECT id, request_hash, status, reason, created_at, resolved_at FROM policy_approval_grants \
                 WHERE status = ? ORDER BY created_at DESC",
            )
            .bind(status)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, ApprovalGrantRow>(
                "SELECT id, request_hash, status, reason, created_at, resolved_at FROM policy_approval_grants \
                 ORDER BY created_at DESC",
            )
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(PolicyApprovalGrant::from).collect())
}

pub async fn resolve_approval_grant(
    pool: &SqlitePool,
    id: &str,
    approved: bool,
) -> Result<PolicyApprovalGrant, DbError> {
    let status = if approved { "approved" } else { "denied" };
    sqlx::query("UPDATE policy_approval_grants SET status = ?, resolved_at = ? WHERE id = ?")
        .bind(status)
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    let row = sqlx::query_as::<_, ApprovalGrantRow>(
        "SELECT id, request_hash, status, reason, created_at, resolved_at FROM policy_approval_grants WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

pub async fn insert_audit_export(
    pool: &SqlitePool,
    format: &str,
    body: &str,
) -> Result<PolicyAuditExport, DbError> {
    let export = PolicyAuditExport {
        id: new_id(),
        created_at: now(),
        format: format.to_string(),
        body: body.to_string(),
    };
    sqlx::query(
        "INSERT INTO policy_audit_exports (id, created_at, format, body) VALUES (?, ?, ?, ?)",
    )
    .bind(&export.id)
    .bind(export.created_at)
    .bind(&export.format)
    .bind(&export.body)
    .execute(pool)
    .await?;
    Ok(export)
}

const USAGE_SELECT: &str = "SELECT id, ts, org_id, team_id, user_id, project_id, group_id, repo_id, \
    session_id, run_id, agent_kind, provider, model, traffic_kind, api_source, source_label, \
    input_tokens, output_tokens, estimated_cost_usd, policy_envelope_id, request_count, status_code \
    FROM usage_ledger";

const BUDGET_SELECT: &str =
    "SELECT id, name, enabled, scope_kind, scope_id, provider, agent_kind, \
    model, traffic_kind, enforce_managed_sessions, enforce_api_gateway, soft_token_cap, \
    hard_token_cap, soft_cost_cap_usd, hard_cost_cap_usd, warning_threshold, window, created_at, \
    updated_at FROM budget_policies";

async fn default_budget_status(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<BudgetStatus>, DbError> {
    let (input, output, cost): (Option<i64>, Option<i64>, Option<f64>) = match project_id {
        Some(project_id) => sqlx::query_as(
            "SELECT SUM(input_tokens), SUM(output_tokens), SUM(estimated_cost_usd) \
             FROM usage_ledger WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_one(pool)
        .await?,
        None => sqlx::query_as(
            "SELECT SUM(input_tokens), SUM(output_tokens), SUM(estimated_cost_usd) FROM usage_ledger",
        )
        .fetch_one(pool)
        .await?,
    };
    let input_tokens = input.unwrap_or_default().max(0) as u64;
    let output_tokens = output.unwrap_or_default().max(0) as u64;
    Ok(vec![BudgetStatus {
        scope: if project_id.is_some() {
            "project"
        } else {
            "organization"
        }
        .into(),
        subject_id: project_id.map(str::to_string),
        provider: None,
        agent: None,
        model: None,
        traffic_kind: None,
        enforce_managed_sessions: false,
        enforce_api_gateway: false,
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        estimated_cost_usd: cost,
        soft_token_cap: None,
        hard_token_cap: None,
        warning: None,
        hard_exceeded: false,
    }])
}

async fn usage_totals_for_budget(
    pool: &SqlitePool,
    policy: &BudgetPolicyRecord,
    project_filter: Option<&str>,
) -> Result<(Option<i64>, Option<i64>, Option<f64>), DbError> {
    let mut qb: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
        "SELECT SUM(input_tokens), SUM(output_tokens), SUM(estimated_cost_usd) FROM usage_ledger WHERE 1 = 1",
    );

    if let Some(project_id) = project_filter {
        qb.push(" AND project_id = ").push_bind(project_id);
    }
    match policy.scope.kind {
        PolicyScopeKind::Organization => {}
        PolicyScopeKind::Project => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND project_id = ").push_bind(id);
            }
        }
        PolicyScopeKind::Group => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND group_id = ").push_bind(id);
            }
        }
        PolicyScopeKind::Repository => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND repo_id = ").push_bind(id);
            }
        }
        PolicyScopeKind::AgentType => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND agent_kind = ").push_bind(id);
            }
        }
        PolicyScopeKind::Session => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND session_id = ").push_bind(id);
            }
        }
        PolicyScopeKind::Run => {
            if let Some(id) = &policy.scope.id {
                qb.push(" AND run_id = ").push_bind(id);
            }
        }
        PolicyScopeKind::Team
        | PolicyScopeKind::User
        | PolicyScopeKind::Branch
        | PolicyScopeKind::TaskType
        | PolicyScopeKind::RuntimeType => {}
    }
    if let Some(provider) = &policy.provider {
        qb.push(" AND provider = ").push_bind(provider);
    }
    if let Some(agent) = policy.agent {
        qb.push(" AND agent_kind = ").push_bind(agent.as_str());
    }
    if let Some(model) = &policy.model {
        qb.push(" AND model = ").push_bind(model);
    }
    if let Some(traffic_kind) = &policy.traffic_kind {
        qb.push(" AND traffic_kind = ").push_bind(traffic_kind);
    } else if !policy.enforce_api_gateway {
        qb.push(" AND traffic_kind != 'api_gateway'");
    } else if !policy.enforce_managed_sessions {
        qb.push(" AND traffic_kind = 'api_gateway'");
    }

    Ok(qb.build_query_as().fetch_one(pool).await?)
}

fn budget_warning(
    policy: &BudgetPolicyRecord,
    total_tokens: u64,
    estimated_cost_usd: Option<f64>,
) -> Option<String> {
    let threshold = policy
        .budget
        .warning_threshold
        .unwrap_or(0.8)
        .clamp(0.0, 1.0);
    if let Some(hard) = policy.budget.hard_token_cap {
        if total_tokens >= hard {
            return Some(format!("hard token cap reached ({total_tokens}/{hard})"));
        }
        if (total_tokens as f64) >= (hard as f64 * threshold) {
            return Some(format!(
                "token usage is above {:.0}% of hard cap",
                threshold * 100.0
            ));
        }
    }
    if let Some(soft) = policy.budget.soft_token_cap {
        if total_tokens >= soft {
            return Some(format!("soft token cap reached ({total_tokens}/{soft})"));
        }
    }
    if let (Some(hard), Some(used)) = (policy.budget.hard_cost_cap_usd, estimated_cost_usd) {
        if used >= hard {
            return Some(format!("hard cost cap reached (${used:.2}/${hard:.2})"));
        }
        if used >= hard * threshold {
            return Some(format!(
                "cost is above {:.0}% of hard cap",
                threshold * 100.0
            ));
        }
    }
    if let (Some(soft), Some(used)) = (policy.budget.soft_cost_cap_usd, estimated_cost_usd) {
        if used >= soft {
            return Some(format!("soft cost cap reached (${used:.2}/${soft:.2})"));
        }
    }
    None
}

fn parse_scope_kind(value: &str) -> Result<PolicyScopeKind, DbError> {
    Ok(match value {
        "organization" => PolicyScopeKind::Organization,
        "group" => PolicyScopeKind::Group,
        "team" => PolicyScopeKind::Team,
        "user" => PolicyScopeKind::User,
        "project" => PolicyScopeKind::Project,
        "repository" => PolicyScopeKind::Repository,
        "branch" => PolicyScopeKind::Branch,
        "task_type" => PolicyScopeKind::TaskType,
        "agent_type" => PolicyScopeKind::AgentType,
        "runtime_type" => PolicyScopeKind::RuntimeType,
        "session" => PolicyScopeKind::Session,
        "run" => PolicyScopeKind::Run,
        _ => return Err(DbError::InvalidEnum(value.to_string())),
    })
}
