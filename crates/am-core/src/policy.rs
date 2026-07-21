use am_agents::AgentPolicyRuntime;
use am_proto::{AgentKind, ExecutionBackend};

use crate::{AppCore, CoreError};

/// Effective launch settings after applying the extension's run preflight.
///
/// The standalone VS Code extension does not ship Perpetual's enterprise
/// policy engine. Keeping this small shape lets the run paths share the same
/// call site while preserving the user's chosen agent, model, backend, and
/// permission behavior.
#[derive(Debug, Clone)]
pub(crate) struct PolicyPreflight {
    pub agent: AgentKind,
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
    pub runtime_policy: AgentPolicyRuntime,
    pub envelope_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PolicyPreflightInput {
    pub agent: AgentKind,
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
}

impl AppCore {
    pub(crate) async fn policy_preflight(
        &self,
        input: PolicyPreflightInput,
    ) -> Result<PolicyPreflight, CoreError> {
        let PolicyPreflightInput {
            agent,
            model,
            runtime,
            ..
        } = input;

        Ok(PolicyPreflight {
            agent,
            model,
            runtime,
            runtime_policy: AgentPolicyRuntime::default(),
            envelope_id: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<u64, CoreError> {
        am_db::repos::usage_ledger::record_tokens(
            &self.db.pool,
            project_id.as_deref(),
            session_id.as_deref(),
            run_id.as_deref(),
            agent,
            model.as_deref(),
            policy_envelope_id.as_deref(),
            input_tokens,
            output_tokens,
        )
        .await?;
        Ok(input_tokens.saturating_add(output_tokens))
    }
}
