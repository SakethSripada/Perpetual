//! AgentManager MCP surface.
//!
//! This crate exposes the WorkNode-shaped contract that managed Codex and
//! Claude Code runs use to coordinate projects through AgentManager. Tools and
//! resources operate directly on the canonical work graph in `am-core`: nodes,
//! edges, repo bindings, runs, context packets, and diffs.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use am_agents::PermissionPolicy;
use am_core::{AppCore, CoreError, WorkRunModelOptions};
use am_proto::{
    AgentKind, ApprovalAsk, ApprovalKind, ExecutionBackend, GateMode, LayoutMode, ModelTargetKind,
    NewLocalRepo, NewProject, NewWorkEdge, NewWorkNode, Project, TaskPriority, TaskStatus,
    WorkEdgeKind, WorkEdgeUpdate, WorkNode, WorkNodeKind, WorkNodeUpdate,
};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, GetPromptRequestParams, GetPromptResult,
    ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, PromptMessageRole, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

pub const MCP_PATH: &str = "/mcp";
pub const MCP_TOKEN_ENV: &str = "AGENTMANAGER_MCP_TOKEN";
pub const MCP_URL_ENV: &str = "AGENTMANAGER_MCP_URL";

const MAX_WAIT_MS: u64 = 30_000;
const MAX_ACTIVITY_LIMIT: i64 = 250;
const DEFAULT_ACTIVITY_LIMIT: i64 = 100;
const DEFAULT_CHILD_NODE_CAP: u32 = 12;
const DEFAULT_CHILD_RUN_CAP: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPolicy {
    #[serde(default)]
    pub allowed_project_id: Option<String>,
    #[serde(default)]
    pub allowed_repo_ids: Vec<String>,
    #[serde(default)]
    pub current_work_node_id: Option<String>,
    #[serde(default)]
    pub current_work_run_id: Option<String>,
    #[serde(default = "default_child_nodes")]
    pub max_child_nodes_per_run: u32,
    #[serde(default = "default_child_runs")]
    pub max_concurrent_child_runs: u32,
    #[serde(default)]
    pub allow_destructive_actions: bool,
    #[serde(default)]
    pub allow_repo_mutation: bool,
    #[serde(default)]
    pub allow_cross_project: bool,
}

impl Default for McpPolicy {
    fn default() -> Self {
        Self {
            allowed_project_id: None,
            allowed_repo_ids: Vec::new(),
            current_work_node_id: None,
            current_work_run_id: None,
            max_child_nodes_per_run: DEFAULT_CHILD_NODE_CAP,
            max_concurrent_child_runs: DEFAULT_CHILD_RUN_CAP,
            allow_destructive_actions: false,
            allow_repo_mutation: false,
            allow_cross_project: false,
        }
    }
}

fn default_child_nodes() -> u32 {
    DEFAULT_CHILD_NODE_CAP
}

fn default_child_runs() -> u32 {
    DEFAULT_CHILD_RUN_CAP
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
}

pub struct McpHttpHandle {
    pub addr: SocketAddr,
    pub endpoint: McpEndpoint,
    cancellation: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl McpHttpHandle {
    pub async fn shutdown(self) {
        self.cancellation.cancel();
        let _ = self.join.await;
    }
}

pub async fn serve_http(
    core: AppCore,
    token: String,
    policy: McpPolicy,
    port: u16,
) -> io::Result<McpHttpHandle> {
    let cancellation = CancellationToken::new();
    let service: StreamableHttpService<AgentManagerMcp, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let core = core.clone();
                let policy = policy.clone();
                move || Ok(AgentManagerMcp::new(core.clone(), policy.clone()))
            },
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_stateful_mode(false)
                .with_json_response(true)
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );

    let token = Arc::new(token);
    // The Claude PreToolUse approval hook posts here; it carries state (the core).
    let approve = axum::Router::new()
        .route("/approve", axum::routing::post(approve_handler))
        .with_state(core.clone());
    let app = axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(approve)
        .nest_service(MCP_PATH, service)
        .layer(middleware::from_fn_with_state(
            token.clone(),
            bearer_auth_middleware,
        ));

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}{MCP_PATH}");
    let join = tokio::spawn({
        let cancellation = cancellation.clone();
        async move {
            if let Err(err) = axum::serve(listener, app)
                .with_graceful_shutdown(async move { cancellation.cancelled_owned().await })
                .await
            {
                tracing::warn!(error = %err, "AgentManager MCP HTTP listener stopped");
            }
        }
    });

    Ok(McpHttpHandle {
        addr,
        endpoint: McpEndpoint {
            url,
            token: (*token).clone(),
        },
        cancellation,
        join,
    })
}

async fn bearer_auth_middleware(
    State(token): State<Arc<String>>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if bearer_matches(req.headers(), &token) {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        "invalid AgentManager MCP bearer token",
    )
        .into_response()
}

fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {token}"))
}

pub async fn stdio_bridge(url: String, token: String) -> Result<(), McpBridgeError> {
    let client = reqwest::Client::new();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = tokio::io::BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = client
            .post(&url)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json")
            .body(trimmed.to_string())
            .send()
            .await?;
        if response.status() == StatusCode::ACCEPTED || response.status() == StatusCode::NO_CONTENT
        {
            continue;
        }
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let value = json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32000,
                    "message": format!("AgentManager MCP HTTP bridge failed: {status}"),
                    "data": body,
                }
            });
            writer.write_all(value.to_string().as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            continue;
        }
        if !body.trim().is_empty() {
            writer.write_all(body.trim().as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum McpBridgeError {
    #[error("stdio bridge I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("stdio bridge HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone)]
pub struct AgentManagerMcp {
    core: AppCore,
    policy: McpPolicy,
    tool_router: ToolRouter<Self>,
}

impl AgentManagerMcp {
    pub fn new(core: AppCore, policy: McpPolicy) -> Self {
        Self {
            core,
            policy,
            tool_router: Self::tool_router(),
        }
    }

    async fn list_nodes(&self, project_id: &str) -> Result<Vec<WorkNode>, CoreError> {
        let mut nodes = self.core.get_work_graph(project_id).await?.nodes;
        nodes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(nodes)
    }

    async fn require_node(&self, node_id: &str) -> Result<WorkNode, CallToolResult> {
        let node = self
            .core
            .get_work_node(node_id)
            .await
            .map_err(|err| tool_error("core_error", err.to_string()))?
            .ok_or_else(|| {
                tool_error("not_found", format!("work node '{node_id}' was not found"))
            })?;
        self.ensure_project_allowed(&node.project_id)?;
        Ok(node)
    }

    fn ensure_project_allowed(&self, project_id: &str) -> Result<(), CallToolResult> {
        if let Some(allowed) = &self.policy.allowed_project_id {
            if allowed != project_id && !self.policy.allow_cross_project {
                return Err(policy_error(format!(
                    "project '{project_id}' is outside this MCP token scope"
                )));
            }
        }
        Ok(())
    }

    fn ensure_project_creation_allowed(&self) -> Result<(), CallToolResult> {
        if self.policy.allow_cross_project || self.policy.allowed_project_id.is_none() {
            Ok(())
        } else {
            Err(policy_error(
                "creating projects requires cross-project MCP policy opt-in",
            ))
        }
    }

    fn ensure_repo_mutation_allowed(&self) -> Result<(), CallToolResult> {
        if self.policy.allow_repo_mutation {
            Ok(())
        } else {
            Err(policy_error(
                "repository mutation requires MCP policy opt-in",
            ))
        }
    }

    fn ensure_repo_scope(&self, repo_ids: &[String]) -> Result<(), CallToolResult> {
        if self.policy.allowed_repo_ids.is_empty() {
            return Ok(());
        }
        let denied: Vec<_> = repo_ids
            .iter()
            .filter(|repo_id| !self.policy.allowed_repo_ids.contains(*repo_id))
            .cloned()
            .collect();
        if denied.is_empty() {
            Ok(())
        } else {
            Err(policy_error(format!(
                "repository ids outside this MCP token scope: {}",
                denied.join(", ")
            )))
        }
    }

    async fn enforce_child_node_cap(&self, project_id: &str) -> Result<(), CallToolResult> {
        let max = self.policy.max_child_nodes_per_run;
        if max == 0 {
            return Err(policy_error(
                "this MCP token may not create child work nodes",
            ));
        }
        if self.policy.current_work_run_id.is_none() && self.policy.current_work_node_id.is_none() {
            return Ok(());
        }
        let count = self
            .list_nodes(project_id)
            .await
            .map_err(|err| tool_error("core_error", err.to_string()))?
            .len() as u32;
        if count >= max {
            return Err(policy_error(format!(
                "child work node cap reached for this MCP run ({max})"
            )));
        }
        Ok(())
    }

    async fn enforce_child_run_cap(&self, project_id: &str) -> Result<(), CallToolResult> {
        let max = self.policy.max_concurrent_child_runs;
        if max == 0 {
            return Err(policy_error("this MCP token may not start child runs"));
        }
        if self.policy.current_work_run_id.is_none() && self.policy.current_work_node_id.is_none() {
            return Ok(());
        }
        let active = self
            .list_nodes(project_id)
            .await
            .map_err(|err| tool_error("core_error", err.to_string()))?
            .into_iter()
            .filter(|node| node.status == TaskStatus::Running)
            .count() as u32;
        if active >= max {
            return Err(policy_error(format!(
                "concurrent child run cap reached for this MCP run ({max})"
            )));
        }
        Ok(())
    }

    async fn audit(
        &self,
        tool: &str,
        project_id: Option<String>,
        node_id: Option<String>,
        ok: bool,
    ) {
        let _ = self
            .core
            .record_mcp_tool_call(project_id, node_id, tool, ok)
            .await;
    }

    async fn project_graph_value(&self, project_id: &str) -> Result<Value, CallToolResult> {
        self.ensure_project_allowed(project_id)?;
        let graph = self
            .core
            .get_work_graph(project_id)
            .await
            .map_err(|err| tool_error("core_error", err.to_string()))?;
        let mut runs = Vec::new();
        for node in &graph.nodes {
            runs.extend(
                self.core
                    .list_work_runs(&node.id)
                    .await
                    .map_err(|err| tool_error("core_error", err.to_string()))?,
            );
        }
        Ok(json!({
            "graph": graph,
            "runs": runs,
            "resources": {
                "work_graph": format!("agentmanager://project/{project_id}/work-graph")
            },
            "summary": "Canonical AgentManager work graph: nodes, edges, repo bindings, and runs."
        }))
    }
}

#[tool_router]
impl AgentManagerMcp {
    #[tool(description = "List AgentManager projects visible to this MCP token")]
    async fn am_list_projects(&self) -> CallToolResult {
        let result = match self.core.list_projects().await {
            Ok(projects) => {
                let projects: Vec<Project> = projects
                    .into_iter()
                    .filter(|project| {
                        self.policy
                            .allowed_project_id
                            .as_ref()
                            .map(|id| id == &project.id || self.policy.allow_cross_project)
                            .unwrap_or(true)
                    })
                    .collect();
                structured(json!({ "projects": projects }))
            }
            Err(err) => tool_error("core_error", err.to_string()),
        };
        self.audit("am_list_projects", None, None, !is_tool_error(&result))
            .await;
        result
    }

    #[tool(description = "Get one AgentManager project by id")]
    async fn am_get_project(
        &self,
        Parameters(input): Parameters<GetProjectInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => match self.core.get_project(&input.project_id).await {
                Ok(Some(project)) => structured(json!({ "project": project })),
                Ok(None) => tool_error("not_found", "project was not found"),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_get_project",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "List repositories connected to a project")]
    async fn am_list_repos(&self, Parameters(input): Parameters<ListReposInput>) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => match self.core.list_repos(&input.project_id).await {
                Ok(repos) => structured(json!({ "repos": repos })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_list_repos",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Create a new AgentManager project when policy allows cross-project creation"
    )]
    async fn am_create_project(
        &self,
        Parameters(input): Parameters<CreateProjectInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_creation_allowed() {
            Err(err) => err,
            Ok(()) => match self
                .core
                .create_project(NewProject {
                    name: input.name,
                    description: input.description,
                })
                .await
            {
                Ok(project) => structured(json!({ "project": project })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit("am_create_project", None, None, !is_tool_error(&result))
            .await;
        result
    }

    #[tool(
        description = "Connect a local repository to a project when policy allows repository mutation"
    )]
    async fn am_connect_local_repo(
        &self,
        Parameters(input): Parameters<ConnectLocalRepoInput>,
    ) -> CallToolResult {
        let result = match self
            .ensure_project_allowed(&input.project_id)
            .and_then(|_| self.ensure_repo_mutation_allowed())
        {
            Err(err) => err,
            Ok(()) => match self
                .core
                .connect_local_repo(NewLocalRepo {
                    project_id: input.project_id.clone(),
                    path: input.path,
                })
                .await
            {
                Ok(repo) => structured(json!({ "repo": repo })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_connect_local_repo",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Read the canonical WorkNode graph for a project")]
    async fn am_get_work_graph(
        &self,
        Parameters(input): Parameters<GetWorkGraphInput>,
    ) -> CallToolResult {
        let result = match self.project_graph_value(&input.project_id).await {
            Ok(value) => structured(value),
            Err(err) => err,
        };
        self.audit(
            "am_get_work_graph",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Create a WorkNode of the requested kind (task, session, group, or milestone; defaults to task). Omit coordinates to let AgentManager auto-place it cleanly."
    )]
    async fn am_create_work_node(
        &self,
        Parameters(input): Parameters<CreateWorkNodeInput>,
    ) -> CallToolResult {
        let result = match self
            .ensure_project_allowed(&input.project_id)
            .and_then(|_| self.ensure_repo_scope(&input.repo_ids))
        {
            Err(err) => err,
            Ok(()) => match self.enforce_child_node_cap(&input.project_id).await {
                Err(err) => err,
                Ok(()) => {
                    match self
                        .core
                        .create_work_node(NewWorkNode {
                            project_id: input.project_id.clone(),
                            parent_id: input.parent_id.clone(),
                            kind: input.kind.as_deref().and_then(WorkNodeKind::parse),
                            title: input.title.clone(),
                            description: input.description.clone().or(input.objective.clone()),
                            priority: input
                                .priority
                                .as_deref()
                                .and_then(TaskPriority::parse)
                                .unwrap_or_default(),
                            primary_agent: input
                                .primary_agent
                                .as_deref()
                                .and_then(AgentKind::parse),
                            model: input.model.clone(),
                            model_target: input
                                .model_target
                                .as_deref()
                                .and_then(ModelTargetKind::parse),
                            compute_profile: input.compute_profile.clone(),
                            max_compute_usd: input.max_compute_usd,
                            allow_auto_purchase: input.allow_auto_purchase,
                            compute_provider: input
                                .compute_provider
                                .as_deref()
                                .and_then(am_proto::ComputeProviderKind::parse),
                            repo_ids: input.repo_ids.clone(),
                            position_x: input.position_x,
                            position_y: input.position_y,
                        })
                        .await
                    {
                        Ok(node) => structured(json!({
                            "work_node": node,
                            "resource": format!("agentmanager://work-node/{}", node.id),
                            "summary": "Created WorkNode in the canonical AgentManager work graph."
                        })),
                        Err(err) => tool_error("core_error", err.to_string()),
                    }
                }
            },
        };
        self.audit(
            "am_create_work_node",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Update mutable fields on a WorkNode")]
    async fn am_update_work_node(
        &self,
        Parameters(input): Parameters<UpdateWorkNodeInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(_) => {
                let patch = WorkNodeUpdate {
                    parent_id: input.parent_id.clone(),
                    title: input.title.clone(),
                    description: input.description.clone().or(input.objective.clone()),
                    status: input.status.as_deref().and_then(TaskStatus::parse),
                    priority: input.priority.as_deref().and_then(TaskPriority::parse),
                    primary_agent: input
                        .primary_agent
                        .as_deref()
                        .or(input.active_agent.as_deref())
                        .and_then(AgentKind::parse),
                    position_x: input.position_x,
                    position_y: input.position_y,
                    sort_order: input.sort_order,
                };
                match self.core.update_work_node(&input.node_id, patch).await {
                    Ok(node) => structured(json!({ "work_node": node })),
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
        };
        self.audit(
            "am_update_work_node",
            project_from_result(&result),
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Move a WorkNode by updating status and visual coordinates when supported"
    )]
    async fn am_move_work_node(
        &self,
        Parameters(input): Parameters<MoveWorkNodeInput>,
    ) -> CallToolResult {
        let result = if input.parent_id.is_some()
            || input.position_x.is_some()
            || input.position_y.is_some()
        {
            match self.require_node(&input.node_id).await {
                Err(err) => err,
                Ok(current) => {
                    let position_x = input.position_x.unwrap_or(current.position_x);
                    let position_y = input.position_y.unwrap_or(current.position_y);
                    match self
                        .core
                        .move_work_node(
                            &input.node_id,
                            input.parent_id.clone(),
                            position_x,
                            position_y,
                        )
                        .await
                    {
                        Ok(node) => structured(json!({ "work_node": node })),
                        Err(err) => tool_error("core_error", err.to_string()),
                    }
                }
            }
        } else if let Some(status) = input.status {
            self.am_update_work_node(Parameters(UpdateWorkNodeInput {
                node_id: input.node_id.clone(),
                status: Some(status),
                ..Default::default()
            }))
            .await
        } else {
            tool_error(
                "invalid_input",
                "provide a status, parent_id, or position to move a work node",
            )
        };
        self.audit(
            "am_move_work_node",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Create an edge between WorkNodes when canonical graph edges are available. Use am_prettify_work_graph after bulk graph edits for a clean readable layout."
    )]
    async fn am_connect_work_nodes(
        &self,
        Parameters(input): Parameters<ConnectWorkNodesInput>,
    ) -> CallToolResult {
        let result = match (
            self.require_node(&input.source_node_id).await,
            self.require_node(&input.target_node_id).await,
        ) {
            (Ok(source), Ok(target)) if source.project_id == target.project_id => {
                let kind = input
                    .kind
                    .as_deref()
                    .and_then(WorkEdgeKind::parse)
                    .unwrap_or(WorkEdgeKind::DependsOn);
                match self
                    .core
                    .connect_work_nodes(NewWorkEdge {
                        project_id: source.project_id.clone(),
                        source_id: input.source_node_id.clone(),
                        target_id: input.target_node_id.clone(),
                        kind,
                        label: input.label.clone(),
                    })
                    .await
                {
                    Ok(edge) => structured(json!({ "edge": edge })),
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
            (Ok(_), Ok(_)) => policy_error(
                "cross-project work edges require policy opt-in and canonical graph support",
            ),
            (Err(err), _) | (_, Err(err)) => err,
        };
        self.audit(
            "am_connect_work_nodes",
            None,
            Some(input.source_node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Update an existing WorkNode edge's endpoints, type, or label")]
    async fn am_update_work_edge(
        &self,
        Parameters(input): Parameters<UpdateWorkEdgeInput>,
    ) -> CallToolResult {
        let patch = WorkEdgeUpdate {
            source_id: input.source_node_id.clone(),
            target_id: input.target_node_id.clone(),
            kind: input.kind.as_deref().and_then(WorkEdgeKind::parse),
            label: input.label.clone(),
        };
        let result = match self.core.update_work_edge(&input.edge_id, patch).await {
            Ok(edge) => structured(json!({ "edge": edge })),
            Err(err) => tool_error("core_error", err.to_string()),
        };
        self.audit("am_update_work_edge", None, None, !is_tool_error(&result))
            .await;
        result
    }

    #[tool(description = "Apply AgentManager's deterministic layout to a project work graph")]
    async fn am_prettify_work_graph(
        &self,
        Parameters(input): Parameters<PrettifyWorkGraphInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => {
                let mode = input
                    .mode
                    .as_deref()
                    .and_then(LayoutMode::parse)
                    .unwrap_or_default();
                match self.core.prettify_work_graph(&input.project_id, mode).await {
                    Ok(graph) => structured(json!({ "graph": graph })),
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
        };
        self.audit(
            "am_prettify_work_graph",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Assign repositories to a WorkNode")]
    async fn am_assign_work_node_repos(
        &self,
        Parameters(input): Parameters<AssignWorkNodeReposInput>,
    ) -> CallToolResult {
        let result = match self.ensure_repo_scope(&input.repo_ids) {
            Err(err) => err,
            Ok(_) => match self
                .core
                .assign_work_node_repos(&input.node_id, input.repo_ids.clone())
                .await
            {
                Ok(bindings) => structured(json!({ "repo_bindings": bindings })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_assign_work_node_repos",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Run an agent on a WorkNode and return the run handle quickly")]
    async fn am_run_work_node(
        &self,
        Parameters(input): Parameters<RunWorkNodeInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => match self.enforce_child_run_cap(&node.project_id).await {
                Err(err) => err,
                Ok(()) => {
                    let agent =
                        match parse_agent_required(input.agent.as_deref(), node.primary_agent) {
                            Ok(agent) => agent,
                            Err(err) => return err,
                        };
                    let permission = parse_permission(input.permission.as_deref());
                    let backend = input
                        .execution_backend
                        .as_deref()
                        .and_then(ExecutionBackend::parse);
                    match self
                        .core
                        .run_work_node_with_model_options(
                            &input.node_id,
                            agent,
                            permission,
                            backend,
                            model_options(
                                input.model.clone(),
                                input.model_target.clone(),
                                input.compute_profile.clone(),
                                input.max_compute_usd,
                                input.allow_auto_purchase.unwrap_or(false),
                            ),
                        )
                        .await
                    {
                        Ok(run_id) => {
                            let queued_followup = if let Some(message) = input
                                .message
                                .as_ref()
                                .map(|message| message.trim())
                                .filter(|message| !message.is_empty())
                            {
                                self.core
                                    .send_work_node_message(
                                        &input.node_id,
                                        agent,
                                        permission,
                                        message.to_string(),
                                    )
                                    .await
                                    .ok()
                                    .is_some()
                            } else {
                                false
                            };
                            structured(json!({
                                "run_id": run_id,
                                "work_node_id": input.node_id,
                                "queued_followup": queued_followup,
                                "resource": format!("agentmanager://work-run/{run_id}"),
                                "summary": "Agent run started. Poll am_get_work_updates or read the work-run resource for progress."
                            }))
                        }
                        Err(err) => tool_error("core_error", err.to_string()),
                    }
                }
            },
        };
        self.audit(
            "am_run_work_node",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Run all ready WorkNodes in a project plan, respecting graph dependencies"
    )]
    async fn am_run_work_plan(
        &self,
        Parameters(input): Parameters<RunWorkPlanInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => {
                let agent =
                    match parse_agent_required(input.agent.as_deref(), Some(AgentKind::ClaudeCode))
                    {
                        Ok(agent) => agent,
                        Err(err) => return err,
                    };
                let permission = parse_permission(input.permission.as_deref());
                let backend = input
                    .execution_backend
                    .as_deref()
                    .and_then(ExecutionBackend::parse);
                let gate_mode = input
                    .gate_mode
                    .as_deref()
                    .and_then(GateMode::parse)
                    .unwrap_or_default();
                let options = am_proto::WorkPlanOptions {
                    failure_mode: input
                        .failure_mode
                        .as_deref()
                        .and_then(am_proto::PlanFailureMode::parse)
                        .unwrap_or_default(),
                    max_node_retries: input.max_node_retries.unwrap_or(0),
                    steer_dependents_on_unblock: input.steer_dependents_on_unblock.unwrap_or(false),
                    model: input.model.clone(),
                    model_target: input
                        .model_target
                        .as_deref()
                        .and_then(ModelTargetKind::parse),
                    compute_profile: input.compute_profile.clone(),
                    max_compute_usd: input.max_compute_usd,
                    allow_auto_purchase: input.allow_auto_purchase.unwrap_or(false),
                };
                match self
                    .core
                    .run_work_plan_with_options(
                        &input.project_id,
                        gate_mode,
                        input.max_active_runs,
                        agent,
                        permission,
                        backend,
                        options,
                    )
                    .await
                {
                    Ok(plan_run) => structured(json!({
                        "plan_run": plan_run,
                        "resource": format!("agentmanager://work-plan-run/{}", plan_run.id),
                        "summary": "Plan run started. Poll am_get_work_updates or inspect the work-plan-run resource for progress."
                    })),
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
        };
        self.audit(
            "am_run_work_plan",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Stop a running WorkPlanRun when destructive actions are allowed")]
    async fn am_stop_work_plan(
        &self,
        Parameters(input): Parameters<StopWorkPlanInput>,
    ) -> CallToolResult {
        let result = if !self.policy.allow_destructive_actions {
            policy_error("stopping a work plan requires destructive-action MCP policy opt-in")
        } else {
            match self.core.stop_work_plan(&input.plan_run_id).await {
                Ok(plan_run) => structured(json!({ "plan_run": plan_run })),
                Err(err) => tool_error("core_error", err.to_string()),
            }
        };
        self.audit("am_stop_work_plan", None, None, !is_tool_error(&result))
            .await;
        result
    }

    #[tool(description = "List WorkPlanRuns for a project")]
    async fn am_list_work_plan_runs(
        &self,
        Parameters(input): Parameters<ListWorkPlanRunsInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => match self.core.list_work_plan_runs(&input.project_id).await {
                Ok(plan_runs) => structured(json!({ "plan_runs": plan_runs })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_list_work_plan_runs",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Get one WorkPlanRun by id")]
    async fn am_get_work_plan_run(
        &self,
        Parameters(input): Parameters<GetWorkPlanRunInput>,
    ) -> CallToolResult {
        let result = match self.core.get_work_plan_run(&input.plan_run_id).await {
            Ok(Some(plan_run)) => match self.ensure_project_allowed(&plan_run.project_id) {
                Err(err) => err,
                Ok(()) => structured(json!({ "plan_run": plan_run })),
            },
            Ok(None) => tool_error("not_found", "work plan run was not found"),
            Err(err) => tool_error("core_error", err.to_string()),
        };
        self.audit("am_get_work_plan_run", None, None, !is_tool_error(&result))
            .await;
        result
    }

    #[tool(description = "Stop a running WorkNode when destructive actions are allowed")]
    async fn am_stop_work_node(
        &self,
        Parameters(input): Parameters<StopWorkNodeInput>,
    ) -> CallToolResult {
        let result = if self.policy.current_work_node_id.as_deref() == Some(input.node_id.as_str())
        {
            policy_error("an MCP run may not stop its own current work node")
        } else if !self.policy.allow_destructive_actions {
            policy_error("stopping work requires destructive-action MCP policy opt-in")
        } else {
            match self.require_node(&input.node_id).await {
                Err(err) => err,
                Ok(_) => match self.core.stop_work_node(&input.node_id).await {
                    Ok(()) => structured(json!({ "stopped": true, "work_node_id": input.node_id })),
                    Err(err) => tool_error("core_error", err.to_string()),
                },
            }
        };
        self.audit(
            "am_stop_work_node",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Send a follow-up message to a WorkNode run, queueing it if the node is active"
    )]
    async fn am_send_work_node_message(
        &self,
        Parameters(input): Parameters<SendWorkNodeMessageInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => {
                let agent = match parse_agent_required(input.agent.as_deref(), node.primary_agent) {
                    Ok(agent) => agent,
                    Err(err) => return err,
                };
                let permission = parse_permission(input.permission.as_deref());
                let options = model_options(
                    input.model.clone(),
                    input.model_target.clone(),
                    input.compute_profile.clone(),
                    input.max_compute_usd,
                    input.allow_auto_purchase.unwrap_or(false),
                );
                if !is_default_model_options(&options) {
                    if let Err(err) = self.core.set_work_node_model_options(&node, &options).await {
                        return tool_error("core_error", err.to_string());
                    }
                }
                match self
                    .core
                    .send_work_node_message(
                        &input.node_id,
                        agent,
                        permission,
                        input.message.clone(),
                    )
                    .await
                {
                    Ok(turn_id) => structured(json!({
                        "turn_id": turn_id,
                        "queued": turn_id.is_none(),
                        "work_node_id": input.node_id
                    })),
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
        };
        self.audit(
            "am_send_work_node_message",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "List WorkNode runs")]
    async fn am_list_work_runs(
        &self,
        Parameters(input): Parameters<ListWorkRunsInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => match self.core.list_work_runs(&node.id).await {
                Ok(runs) => structured(json!({ "runs": runs })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_list_work_runs",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Get one WorkRun by id")]
    async fn am_get_work_run(
        &self,
        Parameters(input): Parameters<GetWorkRunInput>,
    ) -> CallToolResult {
        let run = match self.core.get_work_run(&input.run_id).await {
            Ok(Some(run)) => run,
            Ok(None) => {
                let result = tool_error("not_found", "work run was not found");
                self.audit("am_get_work_run", None, None, false).await;
                return result;
            }
            Err(err) => {
                let result = tool_error("core_error", err.to_string());
                self.audit("am_get_work_run", None, None, false).await;
                return result;
            }
        };
        if let Err(err) = self.require_node(&run.node_id).await {
            self.audit("am_get_work_run", None, Some(run.node_id.clone()), false)
                .await;
            return err;
        }
        let result = structured(json!({
            "run": run,
            "resources": {
                "run": format!("agentmanager://work-run/{}", run.id),
                "transcript": format!("agentmanager://work-node/{}/transcript?tail=80", run.node_id),
            }
        }));
        self.audit("am_get_work_run", None, Some(run.node_id), true)
            .await;
        result
    }

    #[tool(
        description = "List recent project updates after an optional cursor. Can wait briefly for new activity"
    )]
    async fn am_get_work_updates(
        &self,
        Parameters(input): Parameters<GetWorkUpdatesInput>,
    ) -> CallToolResult {
        let result = match input
            .project_id
            .as_deref()
            .map(|project_id| self.ensure_project_allowed(project_id))
            .transpose()
        {
            Err(err) => err,
            Ok(_) => {
                let wait_ms = input.wait_ms.unwrap_or(0).min(MAX_WAIT_MS);
                let limit = input
                    .limit
                    .unwrap_or(DEFAULT_ACTIVITY_LIMIT)
                    .min(MAX_ACTIVITY_LIMIT);
                let mut updates = self
                    .activity_after(input.project_id.as_deref(), input.cursor.as_deref(), limit)
                    .await;
                if matches!(&updates, Ok(items) if items.is_empty()) && wait_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    updates = self
                        .activity_after(input.project_id.as_deref(), input.cursor.as_deref(), limit)
                        .await;
                }
                match updates {
                    Ok(items) => {
                        let next_cursor = items.last().map(|item| item.id.clone());
                        structured(json!({ "updates": items, "next_cursor": next_cursor }))
                    }
                    Err(err) => tool_error("core_error", err.to_string()),
                }
            }
        };
        self.audit(
            "am_get_work_updates",
            input.project_id,
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "List events/transcript entries for a WorkNode")]
    async fn am_list_work_node_events(
        &self,
        Parameters(input): Parameters<ListNodeEventsInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => match self.node_events_value(&node).await {
                Ok(value) => structured(value),
                Err(err) => err,
            },
        };
        self.audit(
            "am_list_work_node_events",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "List provider turns or sessions for a WorkNode")]
    async fn am_list_work_node_turns(
        &self,
        Parameters(input): Parameters<ListNodeTurnsInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => match self.node_turns_value(&node).await {
                Ok(value) => structured(value),
                Err(err) => err,
            },
        };
        self.audit(
            "am_list_work_node_turns",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "List queued follow-up messages for a WorkNode")]
    async fn am_list_queued_work_messages(
        &self,
        Parameters(input): Parameters<ListQueuedMessagesInput>,
    ) -> CallToolResult {
        let result = match self.require_node(&input.node_id).await {
            Err(err) => err,
            Ok(node) => match self.core.list_queued_work_messages(&node.id).await {
                Ok(queued) => structured(json!({ "queued_messages": queued })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_list_queued_work_messages",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Preview the bounded context packet that will be provided to a WorkNode agent"
    )]
    async fn am_preview_context_packet(
        &self,
        Parameters(input): Parameters<PreviewContextInput>,
    ) -> CallToolResult {
        let result = match self.core.preview_context_packet(&input.node_id).await {
            Ok(packet) => structured(json!({
                "context_packet": packet,
                "requested_budget_bytes": input.budget_bytes,
                "resource": format!("agentmanager://work-node/{}/context", input.node_id)
            })),
            Err(err) => tool_error("core_error", err.to_string()),
        };
        self.audit(
            "am_preview_context_packet",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Get the git diff summary for a WorkNode")]
    async fn am_work_node_diff(
        &self,
        Parameters(input): Parameters<WorkNodeDiffInput>,
    ) -> CallToolResult {
        let result = match self.core.work_node_diff(&input.node_id).await {
            Ok(diff) => structured(json!({
                "diff": diff,
                "resource": format!("agentmanager://work-node/{}/diff", input.node_id)
            })),
            Err(err) => tool_error("core_error", err.to_string()),
        };
        self.audit(
            "am_work_node_diff",
            None,
            Some(input.node_id),
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(description = "Search project knowledge, tasks, docs, and memory")]
    async fn am_search_project_knowledge(
        &self,
        Parameters(input): Parameters<SearchProjectKnowledgeInput>,
    ) -> CallToolResult {
        let result = match self.ensure_project_allowed(&input.project_id) {
            Err(err) => err,
            Ok(()) => match self
                .core
                .search(
                    &input.query,
                    Some(&input.project_id),
                    input.limit.unwrap_or(20).min(100),
                )
                .await
            {
                Ok(hits) => structured(json!({ "hits": hits })),
                Err(err) => tool_error("core_error", err.to_string()),
            },
        };
        self.audit(
            "am_search_project_knowledge",
            Some(input.project_id),
            None,
            !is_tool_error(&result),
        )
        .await;
        result
    }

    #[tool(
        description = "Live permission gate. AgentManager asks the user to allow or deny this action and returns the decision. Invoked automatically by Claude via --permission-prompt-tool; not for direct agent use."
    )]
    async fn approval_prompt(
        &self,
        Parameters(input): Parameters<ApprovalPromptInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let run_id = run_id_from_context(&context);
        let ask = build_claude_ask(&input.tool_name, &input.input);
        let decision = self
            .core
            .request_approval_for_run(run_id.as_deref(), AgentKind::ClaudeCode, ask)
            .await;
        // Claude expects the tool result's text content to be a JSON-encoded
        // permission result of the shape {behavior, updatedInput|message}.
        let payload = if decision.is_allow() {
            json!({ "behavior": "allow", "updatedInput": input.input })
        } else {
            json!({ "behavior": "deny", "message": "Denied by the AgentManager user." })
        };
        CallToolResult::success(vec![Content::text(payload.to_string())])
    }
}

impl AgentManagerMcp {
    async fn activity_after(
        &self,
        project_id: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<Vec<am_proto::ActivityEvent>, CoreError> {
        let mut items = self.core.list_activity(project_id, limit).await?;
        items.reverse();
        if let Some(cursor) = cursor {
            if let Some(pos) = items.iter().position(|item| item.id == cursor) {
                items = items.into_iter().skip(pos + 1).collect();
            }
        }
        Ok(items)
    }

    async fn node_events_value(&self, node: &WorkNode) -> Result<Value, CallToolResult> {
        if let Some(thread_id) = &node.thread_id {
            self.core
                .list_thread_events(thread_id)
                .await
                .map(|events| json!({ "events": events }))
                .map_err(|err| tool_error("core_error", err.to_string()))
        } else if let Some(task_id) = &node.task_id {
            self.core
                .list_session_events(task_id)
                .await
                .map(|events| json!({ "events": events }))
                .map_err(|err| tool_error("core_error", err.to_string()))
        } else {
            Ok(json!({
                "events": [],
                "note": "group and milestone work nodes have no agent transcript"
            }))
        }
    }

    async fn node_turns_value(&self, node: &WorkNode) -> Result<Value, CallToolResult> {
        if let Some(thread_id) = &node.thread_id {
            self.core
                .list_thread_turns(thread_id)
                .await
                .map(|turns| json!({ "turns": turns }))
                .map_err(|err| tool_error("core_error", err.to_string()))
        } else if let Some(task_id) = &node.task_id {
            self.core
                .list_sessions(task_id)
                .await
                .map(|sessions| json!({ "sessions": sessions }))
                .map_err(|err| tool_error("core_error", err.to_string()))
        } else {
            Ok(json!({
                "turns": [],
                "note": "group and milestone work nodes have no agent runs"
            }))
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentManagerMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_instructions(
            "Use AgentManager tools to create, inspect, assign, run, and coordinate project work. Long-running actions return handles; poll am_get_work_updates and read resources for large transcripts or diffs.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        let mut resources = Vec::new();
        if let Ok(projects) = self.core.list_projects().await {
            for project in projects {
                if self.ensure_project_allowed(&project.id).is_err() {
                    continue;
                }
                resources.push(
                    RawResource::new(
                        format!("agentmanager://project/{}/work-graph", project.id),
                        format!("{} work graph", project.name),
                    )
                    .with_mime_type("application/json")
                    .with_description("AgentManager canonical WorkNode graph")
                    .optional_annotate(None),
                );
            }
        }
        Ok(ListResourcesResult {
            resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
        match self.read_resource_value(&request.uri).await {
            Ok(value) => Ok(ReadResourceResult::new(vec![ResourceContents::text(
                value.to_string(),
                request.uri,
            )
            .with_mime_type("application/json")])),
            Err(result) => Ok(ReadResourceResult::new(vec![ResourceContents::text(
                result
                    .structured_content
                    .unwrap_or_else(|| json!({"error": "resource_error"}))
                    .to_string(),
                request.uri,
            )
            .with_mime_type("application/json")])),
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListPromptsResult, rmcp::ErrorData> {
        Ok(ListPromptsResult {
            prompts: prompt_catalog(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<GetPromptResult, rmcp::ErrorData> {
        let args = request.arguments.unwrap_or_default();
        prompt_result(&request.name, &args).ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("unknown prompt '{}'", request.name), None)
        })
    }
}

impl AgentManagerMcp {
    async fn read_resource_value(&self, uri: &str) -> Result<Value, CallToolResult> {
        if let Some(project_id) = uri
            .strip_prefix("agentmanager://project/")
            .and_then(|rest| rest.strip_suffix("/work-graph"))
        {
            return self.project_graph_value(project_id).await;
        }
        if let Some(rest) = uri.strip_prefix("agentmanager://work-node/") {
            let (node_id, suffix) = rest
                .split_once('/')
                .map(|(node, suffix)| (node, Some(suffix)))
                .unwrap_or((rest, None));
            let node = self.require_node(node_id).await?;
            return match suffix {
                None => Ok(json!({ "work_node": node })),
                Some("context") => self
                    .core
                    .preview_context_packet(node_id)
                    .await
                    .map(|packet| json!({ "context_packet": packet }))
                    .map_err(|err| tool_error("core_error", err.to_string())),
                Some(s) if s.starts_with("transcript") => {
                    let value = self.node_events_value(&node).await?;
                    let tail = parse_tail(s).unwrap_or(80);
                    let events = value
                        .get("events")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let start = events.len().saturating_sub(tail);
                    Ok(json!({ "events": &events[start..] }))
                }
                Some("diff") => self
                    .core
                    .work_node_diff(node_id)
                    .await
                    .map(|diff| json!({ "diff": diff }))
                    .map_err(|err| tool_error("core_error", err.to_string())),
                Some(_) => Err(tool_error("not_found", "unknown work-node resource suffix")),
            };
        }
        if let Some(run_id) = uri.strip_prefix("agentmanager://work-run/") {
            let result = self
                .am_get_work_run(Parameters(GetWorkRunInput {
                    run_id: run_id.to_string(),
                }))
                .await;
            if let Some(value) = result.structured_content.clone() {
                return Ok(value);
            }
            return Err(result);
        }
        if let Some(plan_run_id) = uri.strip_prefix("agentmanager://work-plan-run/") {
            let result = self
                .am_get_work_plan_run(Parameters(GetWorkPlanRunInput {
                    plan_run_id: plan_run_id.to_string(),
                }))
                .await;
            if let Some(value) = result.structured_content.clone() {
                return Ok(value);
            }
            return Err(result);
        }
        Err(tool_error("not_found", "unknown AgentManager resource URI"))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetProjectInput {
    project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListReposInput {
    project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateProjectInput {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConnectLocalRepoInput {
    project_id: String,
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetWorkGraphInput {
    project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateWorkNodeInput {
    project_id: String,
    title: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    repo_ids: Vec<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    primary_agent: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_target: Option<String>,
    #[serde(default)]
    compute_profile: Option<String>,
    #[serde(default)]
    max_compute_usd: Option<f64>,
    #[serde(default)]
    allow_auto_purchase: Option<bool>,
    #[serde(default)]
    compute_provider: Option<String>,
    #[serde(default)]
    position_x: Option<f64>,
    #[serde(default)]
    position_y: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct UpdateWorkNodeInput {
    node_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    active_agent: Option<String>,
    #[serde(default)]
    primary_agent: Option<String>,
    #[serde(default)]
    position_x: Option<f64>,
    #[serde(default)]
    position_y: Option<f64>,
    #[serde(default)]
    sort_order: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MoveWorkNodeInput {
    node_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    position_x: Option<f64>,
    #[serde(default)]
    position_y: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConnectWorkNodesInput {
    source_node_id: String,
    target_node_id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateWorkEdgeInput {
    edge_id: String,
    #[serde(default)]
    source_node_id: Option<String>,
    #[serde(default)]
    target_node_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PrettifyWorkGraphInput {
    project_id: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AssignWorkNodeReposInput {
    node_id: String,
    repo_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunWorkNodeInput {
    node_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    execution_backend: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_target: Option<String>,
    #[serde(default)]
    compute_profile: Option<String>,
    #[serde(default)]
    max_compute_usd: Option<f64>,
    #[serde(default)]
    allow_auto_purchase: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunWorkPlanInput {
    project_id: String,
    #[serde(default)]
    gate_mode: Option<String>,
    #[serde(default)]
    max_active_runs: Option<i64>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    execution_backend: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_target: Option<String>,
    #[serde(default)]
    compute_profile: Option<String>,
    #[serde(default)]
    max_compute_usd: Option<f64>,
    #[serde(default)]
    allow_auto_purchase: Option<bool>,
    /// halt (default) | continue (skip failed subtrees) | retry (re-queue up
    /// to max_node_retries).
    #[serde(default)]
    failure_mode: Option<String>,
    #[serde(default)]
    max_node_retries: Option<i64>,
    /// Steer already-running dependents with a prerequisite's handoff summary
    /// when it completes (consumes an agent turn per steer).
    #[serde(default)]
    steer_dependents_on_unblock: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StopWorkPlanInput {
    plan_run_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListWorkPlanRunsInput {
    project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetWorkPlanRunInput {
    plan_run_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StopWorkNodeInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendWorkNodeMessageInput {
    node_id: String,
    message: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_target: Option<String>,
    #[serde(default)]
    compute_profile: Option<String>,
    #[serde(default)]
    max_compute_usd: Option<f64>,
    #[serde(default)]
    allow_auto_purchase: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListWorkRunsInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetWorkRunInput {
    run_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetWorkUpdatesInput {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListNodeEventsInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListNodeTurnsInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListQueuedMessagesInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PreviewContextInput {
    node_id: String,
    #[serde(default)]
    budget_bytes: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkNodeDiffInput {
    node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchProjectKnowledgeInput {
    project_id: String,
    query: String,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ApprovalPromptInput {
    /// The tool Claude wants to use.
    tool_name: String,
    /// The proposed tool input.
    #[serde(default)]
    input: Value,
}

/// Extract the AgentManager run id from the per-run `X-AM-Run-Id` header that the
/// Claude MCP config carries, so the approval routes to the right run.
fn run_id_from_context(context: &RequestContext<RoleServer>) -> Option<String> {
    let parts = context.extensions.get::<axum::http::request::Parts>()?;
    parts
        .headers
        .get("x-am-run-id")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// Turn a Claude tool call (name + input) into a provider-independent ask.
fn build_claude_ask(tool_name: &str, input: &Value) -> ApprovalAsk {
    let kind = classify_claude_tool(tool_name);
    let command = if matches!(kind, ApprovalKind::Command) {
        input
            .get("command")
            .and_then(Value::as_str)
            .map(|command| vec![command.to_string()])
    } else {
        None
    };
    ApprovalAsk {
        kind,
        tool_name: tool_name.to_string(),
        command,
        cwd: input
            .get("cwd")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        input: input.clone(),
        reason: None,
    }
}

/// Whether a Claude tool is one of AgentManager's own coordination tools, which
/// are always auto-approved (they are the managed run's control surface).
fn is_agentmanager_tool(tool_name: &str) -> bool {
    tool_name.starts_with("mcp__agentmanager__")
}

/// Whether a Claude tool is a file edit, auto-approved in Edit mode.
fn is_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Create"
    )
}

/// Decide an approval without prompting the user, if possible. AgentManager's own
/// coordination tools are always auto-approved; in `edit` mode file edits are too
/// (Edit mode = auto-approve edits, prompt for the rest). `None` means the action
/// must surface a live approval card.
fn auto_decision(tool_name: &str, mode: &str) -> Option<am_proto::ApprovalDecision> {
    if is_agentmanager_tool(tool_name) || (mode == "edit" && is_edit_tool(tool_name)) {
        Some(am_proto::ApprovalDecision::Allow)
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
struct ApproveQuery {
    #[serde(default)]
    run_id: Option<String>,
    /// `ask` (prompt for everything) or `edit` (auto-approve file edits).
    #[serde(default)]
    mode: Option<String>,
}

/// PreToolUse hook endpoint for Claude live approval. Receives the hook's tool
/// call JSON on the body, surfaces a live approval (unless auto-approved), and
/// returns the hook decision JSON the CLI expects on stdout. Headless `claude -p`
/// ignores `--permission-prompt-tool`, so this hook is how Claude is gated.
async fn approve_handler(
    State(core): State<AppCore>,
    axum::extract::Query(query): axum::extract::Query<ApproveQuery>,
    body: String,
) -> Response {
    let payload: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let mode = query.mode.as_deref().unwrap_or("ask");

    let decision = match auto_decision(&tool_name, mode) {
        Some(decision) => decision,
        None => {
            let ask = build_claude_ask(&tool_name, &tool_input);
            core.request_approval_for_run(query.run_id.as_deref(), AgentKind::ClaudeCode, ask)
                .await
        }
    };

    // Map the decision onto a PreToolUse hook result.
    let hook = if decision.is_allow() {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "Approved in AgentManager."
            }
        })
    } else {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "Denied by the AgentManager user."
            }
        })
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        hook.to_string(),
    )
        .into_response()
}

fn classify_claude_tool(tool: &str) -> ApprovalKind {
    match tool {
        "Bash" | "BashOutput" | "KillShell" => ApprovalKind::Command,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Create" => ApprovalKind::FileChange,
        _ => ApprovalKind::Tool,
    }
}

fn structured(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

fn tool_error(code: impl Into<String>, message: impl Into<String>) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error_code": code.into(),
        "message": message.into(),
    }))
}

fn policy_error(message: impl Into<String>) -> CallToolResult {
    tool_error("policy_denied", message)
}

fn is_tool_error(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
}

fn project_from_result(result: &CallToolResult) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/work_node/project_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn parse_permission(permission: Option<&str>) -> PermissionPolicy {
    match permission.unwrap_or("workspace_write").trim() {
        "read_only" | "read-only" | "plan" => PermissionPolicy::ReadOnly,
        "autonomous" | "dangerous" | "full" => PermissionPolicy::Autonomous,
        _ => PermissionPolicy::WorkspaceWrite,
    }
}

fn parse_agent_required(
    agent: Option<&str>,
    fallback: Option<AgentKind>,
) -> Result<AgentKind, CallToolResult> {
    if let Some(agent) = agent.and_then(AgentKind::parse) {
        return Ok(agent);
    }
    fallback.ok_or_else(|| tool_error("invalid_input", "agent is required for this work node"))
}

fn model_options(
    model: Option<String>,
    model_target: Option<String>,
    compute_profile: Option<String>,
    max_compute_usd: Option<f64>,
    allow_auto_purchase: bool,
) -> WorkRunModelOptions {
    WorkRunModelOptions {
        model: clean_optional(model),
        model_target: model_target.as_deref().and_then(ModelTargetKind::parse),
        compute_profile: clean_optional(compute_profile),
        max_compute_usd,
        allow_auto_purchase,
    }
}

fn is_default_model_options(options: &WorkRunModelOptions) -> bool {
    options.model.is_none()
        && options.model_target.is_none()
        && options.compute_profile.is_none()
        && options.max_compute_usd.is_none()
        && !options.allow_auto_purchase
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_tail(suffix: &str) -> Option<usize> {
    suffix
        .split_once("tail=")
        .and_then(|(_, tail)| tail.parse::<usize>().ok())
}

fn prompt_catalog() -> Vec<Prompt> {
    vec![
        prompt(
            "architect_project",
            "Design an AgentManager project plan and initial work graph.",
        ),
        prompt(
            "break_down_feature",
            "Break a feature into scoped WorkNodes with agent handoffs.",
        ),
        prompt(
            "coordinate_parallel_agents",
            "Coordinate safe parallel runs across non-conflicting WorkNodes.",
        ),
        prompt(
            "review_handoff",
            "Review a completed WorkNode handoff, transcript, and diff.",
        ),
        prompt(
            "summarize_project_status",
            "Summarize project status from the work graph and recent updates.",
        ),
    ]
}

fn prompt(name: &str, description: &str) -> Prompt {
    Prompt::new(
        name,
        Some(description),
        Some(vec![
            PromptArgument::new("project_id")
                .with_description("AgentManager project id")
                .with_required(false),
            PromptArgument::new("work_node_id")
                .with_description("Optional focused WorkNode id")
                .with_required(false),
            PromptArgument::new("goal")
                .with_description("Human goal or feature request")
                .with_required(false),
        ]),
    )
}

fn prompt_result(name: &str, args: &Map<String, Value>) -> Option<GetPromptResult> {
    let project_id = arg(args, "project_id").unwrap_or_else(|| "<project_id>".to_string());
    let work_node_id = arg(args, "work_node_id").unwrap_or_else(|| "<work_node_id>".to_string());
    let goal = arg(args, "goal").unwrap_or_else(|| "<goal>".to_string());
    let text = match name {
        "architect_project" => format!(
            "You are acting as the AgentManager project architect for project {project_id}. Use am_get_work_graph, am_create_work_node, am_assign_work_node_repos, and am_run_work_node to turn this goal into an executable plan: {goal}"
        ),
        "break_down_feature" => format!(
            "Break this feature into WorkNodes under project {project_id}: {goal}. Prefer small, independently runnable nodes with clear repo scope, dependencies, and review criteria."
        ),
        "coordinate_parallel_agents" => format!(
            "Coordinate safe parallel agent execution in project {project_id}. Inspect the graph and repo bindings, start only non-conflicting WorkNodes, and monitor progress with am_get_work_updates."
        ),
        "review_handoff" => format!(
            "Review WorkNode {work_node_id}. Read its transcript, run data, and diff resources; summarize completion, risks, follow-ups, and whether another agent should continue."
        ),
        "summarize_project_status" => format!(
            "Summarize project {project_id}. Use the work graph and recent updates to report active runs, blocked nodes, done work, risky diffs, and the best next actions."
        ),
        _ => return None,
    };
    Some(
        GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)])
            .with_description(format!("AgentManager prompt: {name}")),
    )
}

fn arg(args: &Map<String, Value>, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn test_core() -> AppCore {
        let dir =
            std::env::temp_dir().join(format!("agentmanager-mcp-test-{}", am_proto::new_id()));
        AppCore::new(&dir).await.unwrap()
    }

    #[test]
    fn approval_auto_decision_matrix() {
        use am_proto::ApprovalDecision::Allow;
        // Coordination tools are always auto-approved.
        assert_eq!(
            auto_decision("mcp__agentmanager__am_run_work_node", "ask"),
            Some(Allow)
        );
        assert_eq!(
            auto_decision("mcp__agentmanager__am_run_work_node", "edit"),
            Some(Allow)
        );
        // Edit mode auto-approves file edits but prompts for commands/other tools.
        assert_eq!(auto_decision("Edit", "edit"), Some(Allow));
        assert_eq!(auto_decision("Write", "edit"), Some(Allow));
        assert_eq!(auto_decision("Bash", "edit"), None);
        // Ask mode prompts for everything, edits included.
        assert_eq!(auto_decision("Edit", "ask"), None);
        assert_eq!(auto_decision("Bash", "ask"), None);
        assert_eq!(auto_decision("WebFetch", "ask"), None);
    }

    #[test]
    fn default_policy_is_scoped() {
        let policy = McpPolicy::default();
        assert!(!policy.allow_destructive_actions);
        assert!(!policy.allow_repo_mutation);
        assert_eq!(policy.max_child_nodes_per_run, DEFAULT_CHILD_NODE_CAP);
    }

    #[test]
    fn prompt_catalog_contains_architect_prompt() {
        let prompts = prompt_catalog();
        assert!(prompts
            .iter()
            .any(|prompt| prompt.name == "architect_project"));
    }

    #[tokio::test]
    async fn tool_contract_contains_work_graph_tools() {
        let mcp = AgentManagerMcp::new(test_core().await, McpPolicy::default());
        let tools = mcp.tool_router.list_all();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        for expected in [
            "am_get_work_graph",
            "am_create_work_node",
            "am_prettify_work_graph",
            "am_update_work_edge",
            "am_run_work_node",
            "am_run_work_plan",
            "am_list_work_plan_runs",
            "am_get_work_updates",
            "am_work_node_diff",
            "am_search_project_knowledge",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[tokio::test]
    async fn http_listener_rejects_missing_bearer_token() {
        let handle = serve_http(
            test_core().await,
            "secret-token".to_string(),
            McpPolicy::default(),
            0,
        )
        .await
        .unwrap();
        let response = reqwest::Client::new()
            .post(&handle.endpoint.url)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 401);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn create_work_node_lands_in_canonical_graph() {
        let core = test_core().await;
        let project = core
            .create_project(NewProject {
                name: "Canonical".into(),
                description: None,
            })
            .await
            .unwrap();
        let mcp = AgentManagerMcp::new(core, McpPolicy::default());

        let created = mcp
            .am_create_work_node(Parameters(CreateWorkNodeInput {
                project_id: project.id.clone(),
                title: "Wire MCP".into(),
                parent_id: None,
                kind: Some("task".into()),
                description: Some("verify canonical wiring".into()),
                objective: None,
                repo_ids: Vec::new(),
                priority: None,
                primary_agent: None,
                model: None,
                model_target: None,
                compute_profile: None,
                max_compute_usd: None,
                allow_auto_purchase: None,
                compute_provider: None,
                position_x: None,
                position_y: None,
            }))
            .await;
        assert_ne!(created.is_error, Some(true), "create should succeed");
        let node_id = created
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/work_node/id"))
            .and_then(Value::as_str)
            .expect("created node id")
            .to_string();

        // The node is addressable by its canonical id and shows up in the graph.
        let node = mcp.require_node(&node_id).await.expect("node resolves");
        assert_eq!(node.project_id, project.id);

        let graph = mcp
            .am_get_work_graph(Parameters(GetWorkGraphInput {
                project_id: project.id.clone(),
            }))
            .await;
        assert_ne!(graph.is_error, Some(true));
        let listed = graph
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/graph/nodes"))
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .any(|n| n.get("id").and_then(Value::as_str) == Some(node_id.as_str()))
            })
            .unwrap_or(false);
        assert!(listed, "created node should appear in canonical graph");

        // No runs yet, but the canonical run listing wires through cleanly.
        let runs = mcp
            .am_list_work_runs(Parameters(ListWorkRunsInput {
                node_id: node_id.clone(),
            }))
            .await;
        assert_ne!(runs.is_error, Some(true));
        let count = runs
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/runs"))
            .and_then(Value::as_array)
            .map(|runs| runs.len())
            .unwrap_or(usize::MAX);
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn policy_denies_repo_mutation_by_default() {
        let mcp = AgentManagerMcp::new(test_core().await, McpPolicy::default());
        let result = mcp
            .am_connect_local_repo(Parameters(ConnectLocalRepoInput {
                project_id: "p".into(),
                path: "/tmp/repo".into(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
    }
}
