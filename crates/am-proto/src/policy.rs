use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{new_id, now, AgentKind, ExecutionBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScopeKind {
    Organization,
    Group,
    Team,
    User,
    Project,
    Repository,
    Branch,
    TaskType,
    AgentType,
    RuntimeType,
    Session,
    Run,
}

impl PolicyScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Organization => "organization",
            Self::Group => "group",
            Self::Team => "team",
            Self::User => "user",
            Self::Project => "project",
            Self::Repository => "repository",
            Self::Branch => "branch",
            Self::TaskType => "task_type",
            Self::AgentType => "agent_type",
            Self::RuntimeType => "runtime_type",
            Self::Session => "session",
            Self::Run => "run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyScope {
    pub kind: PolicyScopeKind,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySelector {
    #[serde(default)]
    pub scopes: Vec<PolicyScope>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub traffic_kinds: Vec<String>,
    #[serde(default)]
    pub api_sources: Vec<String>,
    #[serde(default)]
    pub agents: Vec<AgentKind>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub runtime_types: Vec<ExecutionBackend>,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default)]
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub run_ids: Vec<String>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    #[serde(default)]
    pub tool_globs: Vec<String>,
    #[serde(default)]
    pub mcp_server_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Deny {
        #[serde(default)]
        message: Option<String>,
    },
    RequireApproval {
        #[serde(default)]
        approval: Option<ApprovalPolicy>,
    },
    Warn {
        message: String,
    },
    Pause {
        #[serde(default)]
        message: Option<String>,
    },
    Kill {
        #[serde(default)]
        message: Option<String>,
    },
    SubstituteModel {
        model: String,
    },
    SwitchRuntime {
        runtime: ExecutionBackend,
    },
    FallbackAgent {
        agent: AgentKind,
    },
    RestrictTools {
        #[serde(default)]
        allowed: Vec<String>,
        #[serde(default)]
        denied: Vec<String>,
        #[serde(default)]
        allowed_mcp_servers: Vec<String>,
        #[serde(default)]
        denied_mcp_servers: Vec<String>,
    },
    RestrictContext {
        #[serde(default)]
        allowed_globs: Vec<String>,
        #[serde(default)]
        denied_globs: Vec<String>,
        #[serde(default)]
        env_allowlist: Vec<String>,
    },
    EnforceBudget {
        budget: BudgetPolicy,
    },
    Audit {
        audit: AuditPolicy,
    },
}

impl PolicyEffect {
    pub fn kind(&self) -> String {
        match self {
            Self::Allow => "allow",
            Self::Deny { .. } => "deny",
            Self::RequireApproval { .. } => "require_approval",
            Self::Warn { .. } => "warn",
            Self::Pause { .. } => "pause",
            Self::Kill { .. } => "kill",
            Self::SubstituteModel { .. } => "substitute_model",
            Self::SwitchRuntime { .. } => "switch_runtime",
            Self::FallbackAgent { .. } => "fallback_agent",
            Self::RestrictTools { .. } => "restrict_tools",
            Self::RestrictContext { .. } => "restrict_context",
            Self::EnforceBudget { .. } => "enforce_budget",
            Self::Audit { .. } => "audit",
        }
        .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub selector: PolicySelector,
    pub effect: PolicyEffect,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDocument {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PolicyDocument {
    pub fn new_template(
        name: &str,
        description: &str,
        priority: i64,
        rules: Vec<PolicyRule>,
    ) -> Self {
        let ts = now();
        Self {
            id: new_id(),
            name: name.to_string(),
            description: Some(description.to_string()),
            enabled: false,
            priority,
            rules,
            created_at: ts,
            updated_at: ts,
        }
    }

    pub fn starter_review_only() -> Self {
        Self::new_template(
            "Review-only",
            "Keeps review sessions in read-only mode unless a narrower rule overrides it.",
            10,
            vec![PolicyRule::new(
                "Read-only runtime",
                PolicyEffect::RestrictTools {
                    allowed: vec![
                        "Read".into(),
                        "Grep".into(),
                        "Glob".into(),
                        "Bash(git diff *)".into(),
                        "Bash(git status*)".into(),
                    ],
                    denied: vec!["Edit".into(), "Write".into(), "Bash(git push *)".into()],
                    allowed_mcp_servers: vec![],
                    denied_mcp_servers: vec![],
                },
            )],
        )
    }

    pub fn starter_standard_workspace() -> Self {
        Self::new_template(
            "Standard workspace write",
            "Default local development posture: workspace edits, no unrestricted escalation.",
            20,
            vec![PolicyRule::new(
                "Workspace write warnings",
                PolicyEffect::Warn {
                    message: "Workspace-write sessions are allowed and audited.".into(),
                },
            )],
        )
    }

    pub fn starter_docker_only() -> Self {
        Self::new_template(
            "Docker-only",
            "Switches matching sessions to the Docker sandbox runtime.",
            30,
            vec![PolicyRule::new(
                "Use Docker sandbox",
                PolicyEffect::SwitchRuntime {
                    runtime: ExecutionBackend::DockerSandbox,
                },
            )],
        )
    }

    pub fn starter_secrets_safe() -> Self {
        Self::new_template(
            "Secrets-safe",
            "Blocks common secret-bearing files from context and tool access.",
            40,
            vec![PolicyRule::new(
                "Deny secrets",
                PolicyEffect::RestrictContext {
                    allowed_globs: vec![],
                    denied_globs: vec![
                        "**/.env".into(),
                        "**/.env.*".into(),
                        "**/secrets/**".into(),
                        "**/*.pem".into(),
                        "**/*.key".into(),
                    ],
                    env_allowlist: vec!["PATH".into(), "HOME".into(), "TMPDIR".into()],
                },
            )],
        )
    }

    pub fn starter_mcp_locked_down() -> Self {
        Self::new_template(
            "MCP locked down",
            "Allows AgentManager coordination tools but denies unapproved MCP servers.",
            50,
            vec![PolicyRule::new(
                "AgentManager MCP only",
                PolicyEffect::RestrictTools {
                    allowed: vec!["mcp__agentmanager__*".into()],
                    denied: vec!["mcp__*".into()],
                    allowed_mcp_servers: vec!["agentmanager".into()],
                    denied_mcp_servers: vec!["*".into()],
                },
            )],
        )
    }

    pub fn starter_cost_saver() -> Self {
        Self::new_template(
            "Cost saver",
            "Warns and caps token usage for exploratory local sessions.",
            60,
            vec![PolicyRule::new(
                "Token cap",
                PolicyEffect::EnforceBudget {
                    budget: BudgetPolicy {
                        soft_token_cap: Some(80_000),
                        hard_token_cap: Some(120_000),
                        warning_threshold: Some(0.75),
                        ..Default::default()
                    },
                },
            )],
        )
    }

    pub fn starter_local_fallback() -> Self {
        Self::new_template(
            "Local fallback",
            "Permits fallback to local models when cloud providers are unavailable.",
            70,
            vec![PolicyRule::new(
                "Local fallback warning",
                PolicyEffect::Warn {
                    message:
                        "Local model fallback is allowed when network recovery policy triggers."
                            .into(),
                },
            )],
        )
    }

    pub fn starter_protected_branch() -> Self {
        let mut rule = PolicyRule::new(
            "Protect main branches",
            PolicyEffect::RequireApproval {
                approval: Some(ApprovalPolicy {
                    mode: "manual".into(),
                    approver_roles: vec!["maintainer".into()],
                    remember_for_session: false,
                    reason: Some(
                        "Protected branches require approval before agent work starts.".into(),
                    ),
                }),
            },
        );
        rule.selector.branches = vec!["main".into(), "master".into(), "release/*".into()];
        Self::new_template(
            "Protected branch",
            "Requires explicit approval before sessions target protected branch names.",
            80,
            vec![rule],
        )
    }

    pub fn starter_templates() -> Vec<Self> {
        vec![
            Self::starter_review_only(),
            Self::starter_standard_workspace(),
            Self::starter_docker_only(),
            Self::starter_secrets_safe(),
            Self::starter_mcp_locked_down(),
            Self::starter_cost_saver(),
            Self::starter_local_fallback(),
            Self::starter_protected_branch(),
            Self::starter_cloud_guardrails(),
        ]
    }

    pub fn starter_cloud_guardrails() -> Self {
        let mut approval_rule = PolicyRule::new(
            "Approve cloud handoffs",
            PolicyEffect::RequireApproval {
                approval: Some(ApprovalPolicy {
                    mode: "manual".into(),
                    approver_roles: vec!["user".into()],
                    remember_for_session: true,
                    reason: Some(
                        "Cloud continuation sends the working branch to provider-hosted \
                         infrastructure."
                            .into(),
                    ),
                }),
            },
        );
        approval_rule.selector.runtime_types = vec![ExecutionBackend::Cloud];
        let mut warn_rule = PolicyRule::new(
            "Cloud run notice",
            PolicyEffect::Warn {
                message: "This run executes on provider cloud infrastructure and shares the \
                          account's normal rate limits."
                    .into(),
            },
        );
        warn_rule.selector.runtime_types = vec![ExecutionBackend::Cloud];
        Self::new_template(
            "Cloud execution guardrails",
            "Requires approval before work is handed to Codex Cloud or Claude Code on the web. \
             Scope it to repositories whose code must not leave the machine by switching the \
             approval rule's effect to Deny.",
            65,
            vec![approval_rule, warn_rule],
        )
    }
}

impl PolicyRule {
    pub fn new(name: &str, effect: PolicyEffect) -> Self {
        Self {
            id: new_id(),
            name: name.to_string(),
            enabled: true,
            selector: PolicySelector::default(),
            effect,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyBinding {
    pub id: String,
    pub document_id: String,
    pub scope: PolicyScope,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetPolicy {
    #[serde(default)]
    pub soft_token_cap: Option<u64>,
    #[serde(default)]
    pub hard_token_cap: Option<u64>,
    #[serde(default)]
    pub soft_cost_cap_usd: Option<f64>,
    #[serde(default)]
    pub hard_cost_cap_usd: Option<f64>,
    #[serde(default)]
    pub warning_threshold: Option<f64>,
    #[serde(default)]
    pub window: Option<String>,
}

impl BudgetPolicy {
    pub fn min_with(&self, other: &Self) -> Self {
        Self {
            soft_token_cap: min_opt(self.soft_token_cap, other.soft_token_cap),
            hard_token_cap: min_opt(self.hard_token_cap, other.hard_token_cap),
            soft_cost_cap_usd: min_f64_opt(self.soft_cost_cap_usd, other.soft_cost_cap_usd),
            hard_cost_cap_usd: min_f64_opt(self.hard_cost_cap_usd, other.hard_cost_cap_usd),
            warning_threshold: min_f64_opt(self.warning_threshold, other.warning_threshold),
            window: self.window.clone().or_else(|| other.window.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetPolicyRecord {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub scope: PolicyScope,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub traffic_kind: Option<String>,
    #[serde(default = "default_true")]
    pub enforce_managed_sessions: bool,
    #[serde(default)]
    pub enforce_api_gateway: bool,
    pub budget: BudgetPolicy,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BudgetPolicyRecord {
    pub fn new(name: &str, scope: PolicyScope, budget: BudgetPolicy) -> Self {
        let ts = now();
        Self {
            id: new_id(),
            name: name.to_string(),
            enabled: true,
            scope,
            provider: None,
            agent: None,
            model: None,
            traffic_kind: None,
            enforce_managed_sessions: true,
            enforce_api_gateway: false,
            budget,
            created_at: ts,
            updated_at: ts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiGatewayConfig {
    pub id: String,
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub enforce_policies: bool,
    #[serde(default = "default_listen_host")]
    pub listen_host: String,
    #[serde(default)]
    pub listen_port: Option<u16>,
    pub upstream_base_url: String,
    #[serde(default)]
    pub auth_env_var: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiGatewayStatus {
    pub enabled: bool,
    #[serde(default)]
    pub bind_url: Option<String>,
    #[serde(default)]
    pub configs: Vec<ApiGatewayConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    #[serde(default = "default_manual")]
    pub mode: String,
    #[serde(default)]
    pub approver_roles: Vec<String>,
    #[serde(default)]
    pub remember_for_session: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditPolicy {
    #[serde(default = "default_standard")]
    pub level: String,
    #[serde(default)]
    pub export_targets: Vec<String>,
    #[serde(default)]
    pub include_prompts: bool,
    #[serde(default)]
    pub include_tool_inputs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEvaluationRequest {
    pub id: String,
    #[serde(default)]
    pub actor_user_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    pub agent: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
    pub permission: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub traffic_kind: Option<String>,
    #[serde(default)]
    pub api_source: Option<String>,
    #[serde(default)]
    pub requested_paths: Vec<String>,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    #[serde(default)]
    pub requested_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<PolicyScope>,
    #[serde(default)]
    pub prompt_bytes: u64,
    pub created_at: DateTime<Utc>,
}

impl PolicyEvaluationRequest {
    pub fn new(agent: AgentKind, runtime: ExecutionBackend, permission: String) -> Self {
        Self {
            id: new_id(),
            actor_user_id: None,
            org_id: Some("local".into()),
            team_id: None,
            project_id: None,
            group_id: None,
            repo_ids: Vec::new(),
            branch: None,
            task_type: None,
            agent,
            model: None,
            runtime,
            permission,
            session_id: None,
            run_id: None,
            provider: None,
            traffic_kind: Some("managed_session".into()),
            api_source: None,
            requested_paths: Vec::new(),
            requested_tools: Vec::new(),
            requested_mcp_server_ids: Vec::new(),
            scopes: Vec::new(),
            prompt_bytes: 0,
            created_at: now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionAction {
    Allow,
    Deny,
    RequireApproval,
    Warn,
    Pause,
    Kill,
}

impl PolicyDecisionAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::RequireApproval => "require_approval",
            Self::Warn => "warn",
            Self::Pause => "pause",
            Self::Kill => "kill",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "allow" => Self::Allow,
            "deny" => Self::Deny,
            "require_approval" => Self::RequireApproval,
            "warn" => Self::Warn,
            "pause" => Self::Pause,
            "kill" => Self::Kill,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyMatchedRule {
    pub document_id: String,
    pub document_name: String,
    pub rule_id: String,
    pub rule_name: String,
    pub effect: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub id: String,
    pub request_id: String,
    #[serde(default)]
    pub envelope_id: Option<String>,
    pub request: PolicyEvaluationRequest,
    pub action: PolicyDecisionAction,
    #[serde(default)]
    pub effective_agent: Option<AgentKind>,
    #[serde(default)]
    pub effective_model: Option<String>,
    #[serde(default)]
    pub effective_runtime: Option<ExecutionBackend>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub denied_mcp_servers: Vec<String>,
    #[serde(default)]
    pub allowed_context_globs: Vec<String>,
    #[serde(default)]
    pub denied_context_globs: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub budget: Option<BudgetPolicy>,
    #[serde(default)]
    pub approval: Option<ApprovalPolicy>,
    #[serde(default)]
    pub audit: Option<AuditPolicy>,
    #[serde(default)]
    pub matched_rules: Vec<PolicyMatchedRule>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub approvals_required: Vec<ApprovalPolicy>,
    #[serde(default)]
    pub denied_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPolicyEnvelope {
    pub id: String,
    pub request_id: String,
    pub decision_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub actor_user_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub traffic_kind: Option<String>,
    #[serde(default)]
    pub api_source: Option<String>,
    pub agent: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    pub runtime: ExecutionBackend,
    pub permission: String,
    pub action: PolicyDecisionAction,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub denied_mcp_servers: Vec<String>,
    #[serde(default)]
    pub allowed_context_globs: Vec<String>,
    #[serde(default)]
    pub denied_context_globs: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub budget: Option<BudgetPolicy>,
    #[serde(default)]
    pub approval: Option<ApprovalPolicy>,
    #[serde(default)]
    pub audit: Option<AuditPolicy>,
    #[serde(default)]
    pub matched_rules: Vec<PolicyMatchedRule>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub denied_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLedgerEntry {
    pub id: String,
    pub ts: DateTime<Utc>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub traffic_kind: Option<String>,
    #[serde(default)]
    pub api_source: Option<String>,
    #[serde(default)]
    pub source_label: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub policy_envelope_id: Option<String>,
    #[serde(default = "default_request_count")]
    pub request_count: u64,
    #[serde(default)]
    pub status_code: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub scope: String,
    pub subject_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub traffic_kind: Option<String>,
    #[serde(default)]
    pub enforce_managed_sessions: bool,
    #[serde(default)]
    pub enforce_api_gateway: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub soft_token_cap: Option<u64>,
    #[serde(default)]
    pub hard_token_cap: Option<u64>,
    #[serde(default)]
    pub warning: Option<String>,
    pub hard_exceeded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyApprovalGrant {
    pub id: String,
    pub request_hash: String,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyAuditExport {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub format: String,
    pub body: String,
}

fn default_true() -> bool {
    true
}

fn default_request_count() -> u64 {
    1
}

fn default_listen_host() -> String {
    "127.0.0.1".into()
}

fn default_manual() -> String {
    "manual".to_string()
}

fn default_standard() -> String {
    "standard".to_string()
}

fn min_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn min_f64_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}
