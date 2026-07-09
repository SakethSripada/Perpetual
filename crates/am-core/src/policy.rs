use am_agents::{AgentPolicyRuntime, PermissionPolicy};
use am_proto::{
    new_id, now, AgentKind, ApiGatewayConfig, ApiGatewayStatus, BudgetPolicy, BudgetPolicyRecord,
    BudgetStatus, ExecutionBackend, PolicyApprovalGrant, PolicyAuditExport, PolicyDecision,
    PolicyDecisionAction, PolicyDocument, PolicyEvaluationRequest, PolicyScope, PolicyScopeKind,
    SessionPolicyEnvelope, UsageLedgerEntry,
};
use serde_json::json;

use crate::{AppCore, CoreError};

const LOCAL_ORG_ID: &str = "local";
const LOCAL_USER_ID: &str = "local-user";

#[derive(Debug, Clone)]
pub(crate) struct PolicyPreflight {
    pub agent: AgentKind,
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
    pub envelope: SessionPolicyEnvelope,
    pub runtime_policy: AgentPolicyRuntime,
}

#[derive(Debug, Clone)]
pub(crate) struct PolicyPreflightInput {
    pub project_id: Option<String>,
    pub group_id: Option<String>,
    pub repo_ids: Vec<String>,
    pub branch: Option<String>,
    pub task_type: Option<String>,
    pub agent: AgentKind,
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
    pub permission: PermissionPolicy,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub provider: Option<String>,
    pub traffic_kind: Option<String>,
    pub api_source: Option<String>,
    pub requested_paths: Vec<String>,
    pub requested_tools: Vec<String>,
    pub requested_mcp_server_ids: Vec<String>,
    pub prompt_bytes: u64,
}

impl AppCore {
    /// Monotonic counter identifying the current policy configuration. Any
    /// change to policy documents, budgets, approval grants, or gateway
    /// configs bumps it; cached policy decisions are only valid while the
    /// generation they were computed under is still current.
    pub(crate) fn policy_generation(&self) -> u64 {
        self.policy_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn bump_policy_generation(&self) {
        self.policy_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    pub async fn list_policy_documents(&self) -> Result<Vec<PolicyDocument>, CoreError> {
        self.ensure_starter_policy_documents().await?;
        Ok(am_db::repos::policy::list_documents(&self.db.pool).await?)
    }

    pub async fn upsert_policy_document(
        &self,
        document: PolicyDocument,
    ) -> Result<PolicyDocument, CoreError> {
        let document = am_db::repos::policy::upsert_document(&self.db.pool, document).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "policy.document_saved",
            json!({ "policy_id": document.id, "name": document.name, "enabled": document.enabled }),
        )
        .await?;
        Ok(document)
    }

    pub async fn delete_policy_document(&self, id: &str) -> Result<(), CoreError> {
        am_db::repos::policy::delete_document(&self.db.pool, id).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "policy.document_deleted",
            json!({ "policy_id": id }),
        )
        .await?;
        Ok(())
    }

    pub async fn preview_policy_envelope(
        &self,
        request: PolicyEvaluationRequest,
    ) -> Result<SessionPolicyEnvelope, CoreError> {
        let docs = self.policy_documents_for_request(&request).await?;
        let mut decision =
            am_policy::evaluate(&docs, request).map_err(|err| CoreError::Other(err.to_string()))?;
        self.merge_effective_budget(&mut decision).await?;
        Ok(am_policy::envelope_from_decision(decision))
    }

    pub async fn evaluate_policy(
        &self,
        request: PolicyEvaluationRequest,
    ) -> Result<PolicyDecision, CoreError> {
        let docs = self.policy_documents_for_request(&request).await?;
        let mut decision =
            am_policy::evaluate(&docs, request).map_err(|err| CoreError::Other(err.to_string()))?;
        self.merge_effective_budget(&mut decision).await?;
        let envelope = am_policy::envelope_from_decision(decision.clone());
        decision.envelope_id = Some(envelope.id.clone());
        am_db::repos::policy::insert_envelope(&self.db.pool, &envelope).await?;
        am_db::repos::policy::insert_evaluation(&self.db.pool, &decision).await?;
        Ok(decision)
    }

    pub async fn list_policy_evaluations(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PolicyDecision>, CoreError> {
        Ok(am_db::repos::policy::list_evaluations(&self.db.pool, project_id, limit).await?)
    }

    pub async fn list_usage_ledger(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<UsageLedgerEntry>, CoreError> {
        Ok(am_db::repos::policy::list_usage(&self.db.pool, project_id, limit).await?)
    }

    pub async fn list_budget_status(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<BudgetStatus>, CoreError> {
        Ok(am_db::repos::policy::budget_status(&self.db.pool, project_id).await?)
    }

    pub async fn list_budget_policies(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<BudgetPolicyRecord>, CoreError> {
        Ok(am_db::repos::policy::list_budget_policies(&self.db.pool, project_id).await?)
    }

    pub async fn upsert_budget_policy(
        &self,
        policy: BudgetPolicyRecord,
    ) -> Result<BudgetPolicyRecord, CoreError> {
        let policy = am_db::repos::policy::upsert_budget_policy(&self.db.pool, policy).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "policy.budget_saved",
            json!({
                "budget_policy_id": policy.id,
                "name": policy.name,
                "scope": policy.scope.kind.as_str(),
                "scope_id": policy.scope.id.clone(),
                "api_gateway": policy.enforce_api_gateway,
            }),
        )
        .await?;
        Ok(policy)
    }

    pub async fn delete_budget_policy(&self, id: &str) -> Result<(), CoreError> {
        am_db::repos::policy::delete_budget_policy(&self.db.pool, id).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "policy.budget_deleted",
            json!({ "budget_policy_id": id }),
        )
        .await?;
        Ok(())
    }

    async fn policy_documents_for_request(
        &self,
        request: &PolicyEvaluationRequest,
    ) -> Result<Vec<PolicyDocument>, CoreError> {
        let docs = self.list_policy_documents().await?;
        let bindings = am_db::repos::policy::list_bindings(&self.db.pool).await?;
        if bindings.is_empty() {
            return Ok(docs);
        }
        let filtered = docs
            .into_iter()
            .filter(|doc| {
                let doc_bindings: Vec<_> = bindings
                    .iter()
                    .filter(|binding| binding.document_id == doc.id)
                    .collect();
                doc_bindings.is_empty()
                    || doc_bindings.iter().any(|binding| {
                        request.scopes.iter().any(|scope| {
                            scope.kind == binding.scope.kind
                                && (binding.scope.id.is_none()
                                    || binding.scope.id == scope.id
                                    || scope.id.is_none())
                        })
                    })
            })
            .collect();
        Ok(filtered)
    }

    async fn merge_effective_budget(&self, decision: &mut PolicyDecision) -> Result<(), CoreError> {
        if let Some(extra_budget) = self
            .effective_budget_records_for_request(&decision.request)
            .await?
        {
            decision.budget = Some(match decision.budget.take() {
                Some(existing) => existing.min_with(&extra_budget),
                None => extra_budget,
            });
        }
        Ok(())
    }

    async fn effective_budget_records_for_request(
        &self,
        request: &PolicyEvaluationRequest,
    ) -> Result<Option<BudgetPolicy>, CoreError> {
        let policies = am_db::repos::policy::list_budget_policies(
            &self.db.pool,
            request.project_id.as_deref(),
        )
        .await?;
        let mut budget: Option<BudgetPolicy> = None;
        for policy in policies.into_iter().filter(|policy| policy.enabled) {
            if !budget_policy_matches_request(&policy, request) {
                continue;
            }
            budget = Some(match budget {
                Some(current) => current.min_with(&policy.budget),
                None => policy.budget,
            });
        }
        Ok(budget)
    }

    pub async fn list_api_gateway_configs(&self) -> Result<Vec<ApiGatewayConfig>, CoreError> {
        Ok(am_db::repos::policy::list_api_gateway_configs(&self.db.pool).await?)
    }

    pub async fn upsert_api_gateway_config(
        &self,
        config: ApiGatewayConfig,
    ) -> Result<ApiGatewayConfig, CoreError> {
        let config = am_db::repos::policy::upsert_api_gateway_config(&self.db.pool, config).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "gateway.config_saved",
            json!({
                "gateway_config_id": config.id,
                "provider": config.provider,
                "enabled": config.enabled,
                "enforce_policies": config.enforce_policies,
            }),
        )
        .await?;
        Ok(config)
    }

    pub async fn api_gateway_status(&self) -> Result<ApiGatewayStatus, CoreError> {
        let configs = self.list_api_gateway_configs().await?;
        let live_endpoint = self.api_gateway_endpoint.lock().await.clone();
        Ok(ApiGatewayStatus {
            enabled: configs.iter().any(|config| config.enabled),
            bind_url: live_endpoint.or_else(|| {
                configs.iter().find(|config| config.enabled).map(|config| {
                    match config.listen_port {
                        Some(port) => format!("http://{}:{port}", config.listen_host),
                        None => format!("http://{}:<auto>", config.listen_host),
                    }
                })
            }),
            configs,
        })
    }

    pub async fn list_policy_approval_grants(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<PolicyApprovalGrant>, CoreError> {
        Ok(am_db::repos::policy::list_approval_grants(&self.db.pool, status).await?)
    }

    pub async fn resolve_policy_approval(
        &self,
        id: &str,
        approved: bool,
    ) -> Result<PolicyApprovalGrant, CoreError> {
        let grant =
            am_db::repos::policy::resolve_approval_grant(&self.db.pool, id, approved).await?;
        self.bump_policy_generation();
        self.activity(
            None,
            None,
            "policy.approval_resolved",
            json!({ "approval_id": grant.id, "status": grant.status }),
        )
        .await?;
        Ok(grant)
    }

    pub async fn export_policy_audit(
        &self,
        project_id: Option<&str>,
    ) -> Result<PolicyAuditExport, CoreError> {
        let docs = self.list_policy_documents().await?;
        let evaluations = self.list_policy_evaluations(project_id, 500).await?;
        let usage = self.list_usage_ledger(project_id, 500).await?;
        let body = serde_json::to_string_pretty(&json!({
            "exported_at": now(),
            "project_id": project_id,
            "policy_documents": docs,
            "evaluations": evaluations,
            "usage": usage,
        }))
        .map_err(|err| CoreError::Other(err.to_string()))?;
        let export =
            am_db::repos::policy::insert_audit_export(&self.db.pool, "json", &body).await?;
        self.activity(
            project_id.map(str::to_string),
            None,
            "policy.audit_exported",
            json!({ "export_id": export.id }),
        )
        .await?;
        Ok(export)
    }

    async fn ensure_starter_policy_documents(&self) -> Result<(), CoreError> {
        let existing = am_db::repos::policy::list_documents(&self.db.pool).await?;
        if !existing.is_empty() {
            return Ok(());
        }
        for document in PolicyDocument::starter_templates() {
            am_db::repos::policy::upsert_document(&self.db.pool, document).await?;
        }
        Ok(())
    }

    pub(crate) async fn policy_preflight(
        &self,
        input: PolicyPreflightInput,
    ) -> Result<PolicyPreflight, CoreError> {
        let request = self.policy_request(input);
        let docs = self.policy_documents_for_request(&request).await?;
        let mut decision = am_policy::evaluate(&docs, request.clone())
            .map_err(|err| CoreError::Other(err.to_string()))?;
        let effective_budget = self.effective_budget_records_for_request(&request).await?;
        if let Some(extra_budget) = effective_budget {
            decision.budget = Some(match decision.budget.take() {
                Some(existing) => existing.min_with(&extra_budget),
                None => extra_budget,
            });
        }
        let mut envelope = am_policy::envelope_from_decision(decision.clone());
        decision.envelope_id = Some(envelope.id.clone());

        am_db::repos::policy::insert_envelope(&self.db.pool, &envelope).await?;
        am_db::repos::policy::insert_evaluation(&self.db.pool, &decision).await?;

        self.enforce_budget_preflight(&envelope).await?;

        match decision.action {
            PolicyDecisionAction::Deny | PolicyDecisionAction::Kill => {
                let reason = decision
                    .denied_reason
                    .clone()
                    .unwrap_or_else(|| "Denied by policy".into());
                self.activity(
                    request.project_id.clone(),
                    None,
                    "policy.launch_denied",
                    json!({
                        "envelope_id": envelope.id,
                        "reason": reason,
                        "action": decision.action.as_str(),
                    }),
                )
                .await?;
                return Err(CoreError::Other(reason));
            }
            PolicyDecisionAction::Pause => {
                let reason = decision
                    .denied_reason
                    .clone()
                    .unwrap_or_else(|| "Paused by policy".into());
                self.activity(
                    request.project_id.clone(),
                    None,
                    "policy.launch_paused",
                    json!({ "envelope_id": envelope.id, "reason": reason }),
                )
                .await?;
                return Err(CoreError::Other(reason));
            }
            PolicyDecisionAction::RequireApproval => {
                let hash = approval_hash(&envelope);
                let approved =
                    am_db::repos::policy::approved_grant_for_hash(&self.db.pool, &hash).await?;
                if approved.is_none() {
                    let reason = envelope
                        .approval
                        .as_ref()
                        .and_then(|approval| approval.reason.clone())
                        .or_else(|| envelope.denied_reason.clone())
                        .unwrap_or_else(|| "Policy approval required".into());
                    let grant = am_db::repos::policy::create_pending_grant(
                        &self.db.pool,
                        &hash,
                        Some(&reason),
                    )
                    .await?;
                    self.activity(
                        envelope.project_id.clone(),
                        None,
                        "policy.approval_requested",
                        json!({
                            "approval_id": grant.id,
                            "envelope_id": envelope.id,
                            "reason": reason,
                        }),
                    )
                    .await?;
                    return Err(CoreError::Other(format!(
                        "policy approval required: {reason}"
                    )));
                }
            }
            PolicyDecisionAction::Allow | PolicyDecisionAction::Warn => {}
        }

        let agent = decision.effective_agent.unwrap_or(request.agent);
        let model = decision
            .effective_model
            .clone()
            .or_else(|| request.model.clone());
        let runtime = decision.effective_runtime.unwrap_or(request.runtime);
        envelope.agent = agent;
        envelope.model = model.clone();
        envelope.runtime = runtime;

        self.activity(
            envelope.project_id.clone(),
            None,
            "policy.envelope_applied",
            json!({
                "envelope_id": envelope.id,
                "action": envelope.action.as_str(),
                "agent": envelope.agent.as_str(),
                "runtime": envelope.runtime.as_str(),
                "warnings": envelope.warnings,
            }),
        )
        .await?;

        Ok(PolicyPreflight {
            agent,
            model,
            runtime,
            runtime_policy: runtime_policy_from_envelope(&envelope),
            envelope,
        })
    }

    fn policy_request(&self, input: PolicyPreflightInput) -> PolicyEvaluationRequest {
        let mut scopes = vec![
            PolicyScope {
                kind: PolicyScopeKind::Organization,
                id: Some(LOCAL_ORG_ID.into()),
            },
            PolicyScope {
                kind: PolicyScopeKind::User,
                id: Some(LOCAL_USER_ID.into()),
            },
            PolicyScope {
                kind: PolicyScopeKind::AgentType,
                id: Some(input.agent.as_str().into()),
            },
            PolicyScope {
                kind: PolicyScopeKind::RuntimeType,
                id: Some(input.runtime.as_str().into()),
            },
        ];
        if let Some(project_id) = &input.project_id {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Project,
                id: Some(project_id.clone()),
            });
        }
        if let Some(group_id) = &input.group_id {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Group,
                id: Some(group_id.clone()),
            });
        }
        for repo_id in &input.repo_ids {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Repository,
                id: Some(repo_id.clone()),
            });
        }
        if let Some(branch) = &input.branch {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Branch,
                id: Some(branch.clone()),
            });
        }
        if let Some(task_type) = &input.task_type {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::TaskType,
                id: Some(task_type.clone()),
            });
        }
        if let Some(session_id) = &input.session_id {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Session,
                id: Some(session_id.clone()),
            });
        }
        if let Some(run_id) = &input.run_id {
            scopes.push(PolicyScope {
                kind: PolicyScopeKind::Run,
                id: Some(run_id.clone()),
            });
        }

        PolicyEvaluationRequest {
            id: new_id(),
            actor_user_id: Some(LOCAL_USER_ID.into()),
            org_id: Some(LOCAL_ORG_ID.into()),
            team_id: None,
            project_id: input.project_id,
            group_id: input.group_id,
            repo_ids: input.repo_ids,
            branch: input.branch,
            task_type: input.task_type,
            agent: input.agent,
            model: input.model,
            runtime: input.runtime,
            permission: permission_to_string(input.permission),
            session_id: input.session_id,
            run_id: input.run_id,
            provider: input.provider,
            traffic_kind: input
                .traffic_kind
                .or_else(|| Some("managed_session".to_string())),
            api_source: input.api_source,
            requested_paths: input.requested_paths,
            requested_tools: input.requested_tools,
            requested_mcp_server_ids: input.requested_mcp_server_ids,
            scopes,
            prompt_bytes: input.prompt_bytes,
            created_at: now(),
        }
    }

    async fn enforce_budget_preflight(
        &self,
        envelope: &SessionPolicyEnvelope,
    ) -> Result<(), CoreError> {
        let Some(budget) = &envelope.budget else {
            return Ok(());
        };
        let statuses =
            am_db::repos::policy::budget_status(&self.db.pool, envelope.project_id.as_deref())
                .await?;
        if let Some(status) = statuses.iter().find(|status| status.hard_exceeded) {
            let used = status.total_tokens;
            let cap = status.hard_token_cap.unwrap_or_default();
            return Err(CoreError::Other(format!(
                "policy budget hard cap reached for {} ({used}/{cap} tokens)",
                status.scope
            )));
        }
        let total_tokens = statuses
            .iter()
            .filter(|status| {
                status.scope == "project"
                    && status.subject_id.as_deref() == envelope.project_id.as_deref()
            })
            .map(|status| status.total_tokens)
            .max()
            .or_else(|| statuses.first().map(|status| status.total_tokens))
            .unwrap_or_default();
        if let Some(hard) = budget.hard_token_cap {
            if total_tokens >= hard {
                return Err(CoreError::Other(format!(
                    "policy budget hard cap reached ({total_tokens}/{hard} tokens)"
                )));
            }
        }
        Ok(())
    }

    pub(crate) async fn record_token_usage(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
        run_id: Option<String>,
        agent: AgentKind,
        model: Option<String>,
        policy_envelope_id: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), CoreError> {
        let envelope = match policy_envelope_id.as_deref() {
            Some(id) => am_db::repos::policy::get_envelope(&self.db.pool, id).await?,
            None => None,
        };
        let entry = UsageLedgerEntry {
            id: new_id(),
            ts: now(),
            org_id: Some(LOCAL_ORG_ID.into()),
            team_id: None,
            user_id: Some(LOCAL_USER_ID.into()),
            project_id,
            group_id: envelope
                .as_ref()
                .and_then(|envelope| envelope.group_id.clone()),
            repo_id: None,
            session_id,
            run_id,
            agent: Some(agent),
            provider: envelope
                .as_ref()
                .and_then(|envelope| envelope.provider.clone())
                .or_else(|| Some(agent_provider(agent).into())),
            model,
            traffic_kind: envelope
                .as_ref()
                .and_then(|envelope| envelope.traffic_kind.clone())
                .or_else(|| Some("managed_session".into())),
            api_source: envelope
                .as_ref()
                .and_then(|envelope| envelope.api_source.clone()),
            source_label: Some("Managed agent session".into()),
            input_tokens,
            output_tokens,
            estimated_cost_usd: None,
            policy_envelope_id,
            request_count: 1,
            status_code: None,
        };
        am_db::repos::policy::insert_usage(&self.db.pool, &entry).await?;
        Ok(())
    }
}

fn runtime_policy_from_envelope(envelope: &SessionPolicyEnvelope) -> AgentPolicyRuntime {
    AgentPolicyRuntime {
        allowed_tools: envelope.allowed_tools.clone(),
        denied_tools: envelope.denied_tools.clone(),
        allowed_mcp_servers: envelope.allowed_mcp_servers.clone(),
        denied_mcp_servers: envelope.denied_mcp_servers.clone(),
        denied_context_globs: envelope.denied_context_globs.clone(),
        env_allowlist: envelope.env_allowlist.clone(),
        strict_mcp_config: !envelope.allowed_mcp_servers.is_empty()
            || !envelope.denied_mcp_servers.is_empty(),
        disable_remote_mcp_connectors: envelope
            .denied_mcp_servers
            .iter()
            .any(|server| server == "*" || server == "claude.ai"),
        max_budget_usd: envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.hard_cost_cap_usd),
    }
}

fn budget_policy_matches_request(
    policy: &BudgetPolicyRecord,
    request: &PolicyEvaluationRequest,
) -> bool {
    let is_api_gateway = request.traffic_kind.as_deref() == Some("api_gateway");
    if is_api_gateway && !policy.enforce_api_gateway {
        return false;
    }
    if !is_api_gateway && !policy.enforce_managed_sessions {
        return false;
    }
    if let Some(provider) = policy.provider.as_deref() {
        if request.provider.as_deref() != Some(provider) {
            return false;
        }
    }
    if let Some(agent) = policy.agent {
        if request.agent != agent {
            return false;
        }
    }
    if let Some(model) = policy.model.as_deref() {
        if request.model.as_deref() != Some(model) {
            return false;
        }
    }
    if let Some(traffic_kind) = policy.traffic_kind.as_deref() {
        if request.traffic_kind.as_deref() != Some(traffic_kind) {
            return false;
        }
    }
    request.scopes.iter().any(|scope| {
        scope.kind == policy.scope.kind
            && (policy.scope.id.is_none() || scope.id == policy.scope.id || scope.id.is_none())
    })
}

fn permission_to_string(permission: PermissionPolicy) -> String {
    match permission {
        PermissionPolicy::ReadOnly => "read_only",
        PermissionPolicy::WorkspaceWrite => "workspace_write",
        PermissionPolicy::Ask => "ask",
        PermissionPolicy::Autonomous => "autonomous",
    }
    .to_string()
}

fn approval_hash(envelope: &SessionPolicyEnvelope) -> String {
    let raw = serde_json::to_string(&json!({
        "project_id": envelope.project_id,
        "repo_ids": envelope.repo_ids,
        "branch": envelope.branch,
        "task_type": envelope.task_type,
        "agent": envelope.agent.as_str(),
        "model": envelope.model,
        "runtime": envelope.runtime.as_str(),
        "matched_rules": envelope.matched_rules,
    }))
    .unwrap_or_default();
    stable_hex_hash(raw.as_bytes())
}

fn stable_hex_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn agent_provider(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "anthropic",
        AgentKind::Codex => "openai",
        AgentKind::Gemini => "google",
        AgentKind::Cursor => "cursor",
        AgentKind::OpenCode => "open_code",
    }
}
