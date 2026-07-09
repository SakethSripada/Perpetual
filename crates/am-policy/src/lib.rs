//! Local-first policy evaluation for AgentManager.
//!
//! The engine is intentionally pure: it takes policy documents plus an
//! evaluation request and returns a decision/envelope-ready result. Persistence,
//! approvals, budget ledgers, and process control live in `am-core`/`am-db`.

use std::collections::{BTreeSet, HashSet};

use am_proto::{
    new_id, now, BudgetPolicy, PolicyDecision, PolicyDecisionAction, PolicyDocument, PolicyEffect,
    PolicyEvaluationRequest, PolicyMatchedRule, PolicySelector, SessionPolicyEnvelope,
};

#[derive(Debug, thiserror::Error)]
pub enum PolicyEngineError {
    #[error("policy transform did not converge")]
    TransformDidNotConverge,
}

/// Evaluate all enabled policy documents against a launch/run request.
pub fn evaluate(
    documents: &[PolicyDocument],
    request: PolicyEvaluationRequest,
) -> Result<PolicyDecision, PolicyEngineError> {
    let mut current = request;
    let mut transformed_once = false;

    loop {
        let decision = evaluate_once(documents, current.clone());
        let mut next = current.clone();
        let mut transformed = false;

        if let Some(model) = decision.effective_model.clone() {
            if next.model.as_deref() != Some(model.as_str()) {
                next.model = Some(model);
                transformed = true;
            }
        }
        if let Some(runtime) = decision.effective_runtime {
            if next.runtime != runtime {
                next.runtime = runtime;
                transformed = true;
            }
        }
        if let Some(agent) = decision.effective_agent {
            if next.agent != agent {
                next.agent = agent;
                transformed = true;
            }
        }

        if !transformed {
            return Ok(decision);
        }
        if transformed_once {
            return Err(PolicyEngineError::TransformDidNotConverge);
        }
        current = next;
        transformed_once = true;
    }
}

/// Build the immutable envelope persisted on sessions/runs from an evaluation
/// decision.
pub fn envelope_from_decision(mut decision: PolicyDecision) -> SessionPolicyEnvelope {
    let envelope_id = new_id();
    decision.envelope_id = Some(envelope_id.clone());
    SessionPolicyEnvelope {
        id: envelope_id,
        request_id: decision.request_id.clone(),
        decision_id: decision.id.clone(),
        created_at: now(),
        actor_user_id: decision.request.actor_user_id.clone(),
        org_id: decision.request.org_id.clone(),
        team_id: decision.request.team_id.clone(),
        project_id: decision.request.project_id.clone(),
        group_id: decision.request.group_id.clone(),
        repo_ids: decision.request.repo_ids.clone(),
        branch: decision.request.branch.clone(),
        task_type: decision.request.task_type.clone(),
        session_id: decision.request.session_id.clone(),
        run_id: decision.request.run_id.clone(),
        provider: decision.request.provider.clone(),
        traffic_kind: decision.request.traffic_kind.clone(),
        api_source: decision.request.api_source.clone(),
        agent: decision.effective_agent.unwrap_or(decision.request.agent),
        model: decision
            .effective_model
            .clone()
            .or_else(|| decision.request.model.clone()),
        runtime: decision
            .effective_runtime
            .unwrap_or(decision.request.runtime),
        permission: decision.request.permission.clone(),
        action: decision.action,
        allowed_tools: decision.allowed_tools.clone(),
        denied_tools: decision.denied_tools.clone(),
        allowed_mcp_servers: decision.allowed_mcp_servers.clone(),
        denied_mcp_servers: decision.denied_mcp_servers.clone(),
        allowed_context_globs: decision.allowed_context_globs.clone(),
        denied_context_globs: decision.denied_context_globs.clone(),
        env_allowlist: decision.env_allowlist.clone(),
        budget: decision.budget.clone(),
        approval: decision.approval.clone(),
        audit: decision.audit.clone(),
        matched_rules: decision.matched_rules,
        warnings: decision.warnings,
        denied_reason: decision.denied_reason,
    }
}

fn evaluate_once(documents: &[PolicyDocument], request: PolicyEvaluationRequest) -> PolicyDecision {
    let mut docs: Vec<&PolicyDocument> = documents.iter().filter(|doc| doc.enabled).collect();
    docs.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut matched_rules = Vec::new();
    let mut warnings = Vec::new();
    let mut approvals = Vec::new();
    let mut denied_reason = None;
    let mut action = PolicyDecisionAction::Allow;
    let mut effective_agent = Some(request.agent);
    let mut effective_model = request.model.clone();
    let mut effective_runtime = Some(request.runtime);
    let mut allowed_tools: Option<BTreeSet<String>> = None;
    let mut denied_tools: BTreeSet<String> = BTreeSet::new();
    let mut allowed_mcp_servers: Option<BTreeSet<String>> = None;
    let mut denied_mcp_servers: BTreeSet<String> = BTreeSet::new();
    let mut allowed_context_globs: Option<BTreeSet<String>> = None;
    let mut denied_context_globs: BTreeSet<String> = BTreeSet::new();
    let mut env_allowlist: Option<BTreeSet<String>> = None;
    let mut budget: Option<BudgetPolicy> = None;
    let mut approval = None;
    let mut audit = None;

    for doc in docs {
        for rule in doc.rules.iter().filter(|rule| rule.enabled) {
            if !selector_matches(&rule.selector, &request) {
                continue;
            }
            matched_rules.push(PolicyMatchedRule {
                document_id: doc.id.clone(),
                document_name: doc.name.clone(),
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                effect: rule.effect.kind(),
                reason: rule.reason.clone(),
            });
            match &rule.effect {
                PolicyEffect::Allow => {}
                PolicyEffect::Deny { message } => {
                    action = PolicyDecisionAction::Deny;
                    denied_reason = Some(
                        message
                            .clone()
                            .or_else(|| rule.reason.clone())
                            .unwrap_or_else(|| "Denied by policy".to_string()),
                    );
                }
                PolicyEffect::RequireApproval { approval: next } => {
                    if action == PolicyDecisionAction::Allow {
                        action = PolicyDecisionAction::RequireApproval;
                    }
                    if let Some(next) = next.clone() {
                        approval = Some(next.clone());
                        approvals.push(next);
                    }
                }
                PolicyEffect::Warn { message } => {
                    warnings.push(message.clone());
                    if action == PolicyDecisionAction::Allow {
                        action = PolicyDecisionAction::Warn;
                    }
                }
                PolicyEffect::Pause { message } => {
                    action = PolicyDecisionAction::Pause;
                    denied_reason = Some(
                        message
                            .clone()
                            .or_else(|| rule.reason.clone())
                            .unwrap_or_else(|| "Paused by policy".to_string()),
                    );
                }
                PolicyEffect::Kill { message } => {
                    action = PolicyDecisionAction::Kill;
                    denied_reason = Some(
                        message
                            .clone()
                            .or_else(|| rule.reason.clone())
                            .unwrap_or_else(|| "Stopped by policy".to_string()),
                    );
                }
                PolicyEffect::SubstituteModel { model } => {
                    effective_model = Some(model.clone());
                }
                PolicyEffect::SwitchRuntime { runtime } => {
                    effective_runtime = Some(*runtime);
                }
                PolicyEffect::FallbackAgent { agent } => {
                    effective_agent = Some(*agent);
                }
                PolicyEffect::RestrictTools {
                    allowed,
                    denied,
                    allowed_mcp_servers: allowed_mcp,
                    denied_mcp_servers: denied_mcp,
                } => {
                    intersect_set(&mut allowed_tools, allowed);
                    denied_tools.extend(denied.iter().cloned());
                    intersect_set(&mut allowed_mcp_servers, allowed_mcp);
                    denied_mcp_servers.extend(denied_mcp.iter().cloned());
                }
                PolicyEffect::RestrictContext {
                    allowed_globs,
                    denied_globs,
                    env_allowlist: env,
                } => {
                    intersect_set(&mut allowed_context_globs, allowed_globs);
                    denied_context_globs.extend(denied_globs.iter().cloned());
                    intersect_set(&mut env_allowlist, env);
                }
                PolicyEffect::EnforceBudget { budget: next } => {
                    budget = Some(match budget {
                        Some(current) => current.min_with(next),
                        None => next.clone(),
                    });
                }
                PolicyEffect::Audit { audit: next } => {
                    audit = Some(next.clone());
                }
            }
        }
    }

    if matches!(
        action,
        PolicyDecisionAction::Deny | PolicyDecisionAction::Kill
    ) {
        warnings.clear();
    }

    PolicyDecision {
        id: new_id(),
        request_id: request.id.clone(),
        envelope_id: None,
        request,
        action,
        effective_agent,
        effective_model,
        effective_runtime,
        allowed_tools: allowed_tools.map(set_to_vec).unwrap_or_default(),
        denied_tools: set_to_vec(denied_tools),
        allowed_mcp_servers: allowed_mcp_servers.map(set_to_vec).unwrap_or_default(),
        denied_mcp_servers: set_to_vec(denied_mcp_servers),
        allowed_context_globs: allowed_context_globs.map(set_to_vec).unwrap_or_default(),
        denied_context_globs: set_to_vec(denied_context_globs),
        env_allowlist: env_allowlist.map(set_to_vec).unwrap_or_default(),
        budget,
        approval,
        audit,
        matched_rules,
        warnings,
        approvals_required: approvals,
        denied_reason,
        created_at: now(),
    }
}

fn intersect_set(target: &mut Option<BTreeSet<String>>, next: &[String]) {
    if next.is_empty() {
        return;
    }
    let next_set: BTreeSet<String> = next.iter().cloned().collect();
    match target {
        Some(existing) => {
            *existing = existing.intersection(&next_set).cloned().collect();
        }
        None => *target = Some(next_set),
    }
}

fn set_to_vec(set: BTreeSet<String>) -> Vec<String> {
    set.into_iter().collect()
}

fn selector_matches(selector: &PolicySelector, request: &PolicyEvaluationRequest) -> bool {
    if !selector.scopes.is_empty() {
        let request_scopes: HashSet<_> = request.scopes.iter().collect();
        if selector
            .scopes
            .iter()
            .any(|scope| !request_scopes.contains(scope))
        {
            return false;
        }
    }
    if !selector.providers.is_empty() {
        let Some(provider) = request.provider.as_deref() else {
            return false;
        };
        if !selector
            .providers
            .iter()
            .any(|pattern| wildcard(pattern, provider))
        {
            return false;
        }
    }
    if !selector.traffic_kinds.is_empty() {
        let Some(traffic_kind) = request.traffic_kind.as_deref() else {
            return false;
        };
        if !selector
            .traffic_kinds
            .iter()
            .any(|pattern| wildcard(pattern, traffic_kind))
        {
            return false;
        }
    }
    if !selector.api_sources.is_empty() {
        let Some(api_source) = request.api_source.as_deref() else {
            return false;
        };
        if !selector
            .api_sources
            .iter()
            .any(|pattern| wildcard(pattern, api_source))
        {
            return false;
        }
    }
    if !selector.agents.is_empty() && !selector.agents.contains(&request.agent) {
        return false;
    }
    if !selector.models.is_empty() {
        let Some(model) = request.model.as_deref() else {
            return false;
        };
        if !selector
            .models
            .iter()
            .any(|pattern| wildcard(pattern, model))
        {
            return false;
        }
    }
    if !selector.runtime_types.is_empty() && !selector.runtime_types.contains(&request.runtime) {
        return false;
    }
    if !selector.repo_ids.is_empty()
        && !request
            .repo_ids
            .iter()
            .any(|repo_id| selector.repo_ids.contains(repo_id))
    {
        return false;
    }
    if !selector.branches.is_empty() {
        let Some(branch) = request.branch.as_deref() else {
            return false;
        };
        if !selector
            .branches
            .iter()
            .any(|pattern| wildcard(pattern, branch))
        {
            return false;
        }
    }
    if !selector.task_types.is_empty() {
        let Some(task_type) = request.task_type.as_deref() else {
            return false;
        };
        if !selector
            .task_types
            .iter()
            .any(|pattern| wildcard(pattern, task_type))
        {
            return false;
        }
    }
    if !selector.session_ids.is_empty() {
        let Some(session_id) = request.session_id.as_deref() else {
            return false;
        };
        if !selector.session_ids.iter().any(|id| id == session_id) {
            return false;
        }
    }
    if !selector.run_ids.is_empty() {
        let Some(run_id) = request.run_id.as_deref() else {
            return false;
        };
        if !selector.run_ids.iter().any(|id| id == run_id) {
            return false;
        }
    }
    if !selector.path_globs.is_empty()
        && !request.requested_paths.iter().any(|path| {
            selector
                .path_globs
                .iter()
                .any(|pattern| wildcard(pattern, path))
        })
    {
        return false;
    }
    if !selector.tool_globs.is_empty()
        && !request.requested_tools.iter().any(|tool| {
            selector
                .tool_globs
                .iter()
                .any(|pattern| wildcard(pattern, tool))
        })
    {
        return false;
    }
    if !selector.mcp_server_ids.is_empty()
        && !request.requested_mcp_server_ids.iter().any(|server_id| {
            selector
                .mcp_server_ids
                .iter()
                .any(|pattern| wildcard(pattern, server_id))
        })
    {
        return false;
    }
    true
}

fn wildcard(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_i = 0usize;

    while vi < v.len() {
        if pi < p.len() && (p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            match_i = vi;
            pi += 1;
        } else if let Some(star_i) = star {
            pi = star_i + 1;
            match_i += 1;
            vi = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use am_proto::{AgentKind, ExecutionBackend, PolicyScope, PolicyScopeKind};

    use super::*;

    fn request() -> PolicyEvaluationRequest {
        PolicyEvaluationRequest {
            id: "req".into(),
            actor_user_id: Some("u1".into()),
            org_id: Some("org".into()),
            team_id: None,
            project_id: Some("p1".into()),
            group_id: None,
            repo_ids: vec!["r1".into()],
            branch: Some("main".into()),
            task_type: Some("review".into()),
            agent: AgentKind::Codex,
            model: Some("gpt-5.5".into()),
            runtime: ExecutionBackend::Host,
            permission: "workspace_write".into(),
            session_id: None,
            run_id: None,
            provider: Some("openai".into()),
            traffic_kind: Some("managed_session".into()),
            api_source: None,
            requested_paths: Vec::new(),
            requested_tools: Vec::new(),
            requested_mcp_server_ids: Vec::new(),
            scopes: vec![PolicyScope {
                kind: PolicyScopeKind::Project,
                id: Some("p1".into()),
            }],
            prompt_bytes: 120,
            created_at: now(),
        }
    }

    #[test]
    fn restrict_context_keeps_launch_allowed() {
        let mut doc = PolicyDocument::starter_secrets_safe();
        doc.enabled = true;
        let decision = evaluate(&[doc], request()).unwrap();
        assert!(matches!(decision.action, PolicyDecisionAction::Allow));
        assert!(decision
            .denied_context_globs
            .iter()
            .any(|glob| glob.contains(".env")));
    }

    #[test]
    fn wildcard_matches() {
        assert!(wildcard("release/*", "release/2026-07"));
        assert!(wildcard("*.env", ".env"));
        assert!(!wildcard("main", "develop"));
    }

    #[test]
    fn selector_matches_provider_tools_paths_and_mcp() {
        let mut doc = PolicyDocument::starter_review_only();
        doc.enabled = true;
        doc.rules[0].selector.providers = vec!["openai".into()];
        doc.rules[0].selector.traffic_kinds = vec!["managed_*".into()];
        doc.rules[0].selector.path_globs = vec!["src/*.rs".into()];
        doc.rules[0].selector.tool_globs = vec!["Bash(*)".into()];
        doc.rules[0].selector.mcp_server_ids = vec!["agentmanager".into()];
        let mut req = request();
        req.requested_paths = vec!["src/lib.rs".into()];
        req.requested_tools = vec!["Bash(cargo test)".into()];
        req.requested_mcp_server_ids = vec!["agentmanager".into()];
        let decision = evaluate(&[doc.clone()], req.clone()).unwrap();
        assert_eq!(decision.matched_rules.len(), 1);

        req.requested_tools = vec!["Read".into()];
        let decision = evaluate(&[doc], req).unwrap();
        assert_eq!(decision.matched_rules.len(), 0);
    }
}
