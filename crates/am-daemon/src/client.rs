//! A reusable async client for the daemon. Connects, authenticates, and then
//! multiplexes correlated request/response RPC with a live event stream over
//! one connection. A background reader task routes each server line to either
//! the waiting request or the event broadcast.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use am_agents::PermissionPolicy;
use am_proto::{
    ActivityEvent, AgentKind, AppEvent, ApprovalDecision, ApprovalRequest, CloudAvailability,
    CloudPolicy, CloudRun, ContextPacket, ExecutionBackend, LocalModelPolicy, NewProject,
    NewWorkEdge, NewWorkNode, Project, SearchHit, Task, WorkEdge, WorkGraph, WorkNode,
    WorkNodeDiff, WorkNodeRepoBinding, WorkNodeUpdate,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::protocol::{
    DaemonRequest, DaemonResponse, Handshake, HandshakeAck, RpcRequest, ServerMessage,
};
use crate::write_line;

/// Errors a [`DaemonClient`] can surface.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("daemon error: {0}")]
    Server(String),
    #[error("unexpected response for request")]
    Unexpected,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<DaemonResponse, String>>>>>;

/// A connected, authenticated client.
pub struct DaemonClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<AppEvent>,
    reader_task: JoinHandle<()>,
}

impl Drop for DaemonClient {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

impl DaemonClient {
    /// Connect to a daemon at `addr` and authenticate with `token`.
    pub async fn connect(addr: SocketAddr, token: &str) -> Result<Self, ClientError> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true).ok();
        let (read_half, mut write_half) = stream.into_split();

        write_line(
            &mut write_half,
            &Handshake {
                token: token.to_string(),
                capabilities: vec![crate::protocol::CAP_EVENTS_V2.to_string()],
            },
        )
        .await?;

        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err(ClientError::Auth(
                "connection closed during handshake".into(),
            ));
        }
        let ack: HandshakeAck = serde_json::from_str(line.trim())?;
        if !ack.ok {
            return Err(ClientError::Auth(
                ack.error.unwrap_or_else(|| "rejected".into()),
            ));
        }

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(1024);

        let reader_task = {
            let pending = pending.clone();
            let events = events.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<ServerMessage>(trimmed) {
                        Ok(ServerMessage::Response { id, ok, err }) => {
                            if let Some(tx) = pending.lock().await.remove(&id) {
                                let result = match (ok, err) {
                                    (Some(ok), _) => Ok(ok),
                                    (None, Some(err)) => Err(err),
                                    (None, None) => Err("empty response".into()),
                                };
                                let _ = tx.send(result);
                            }
                        }
                        Ok(ServerMessage::Event(ev)) => {
                            let _ = events.send(ev);
                        }
                        Ok(ServerMessage::EventV2(sequenced)) => {
                            let _ = events.send(sequenced.event);
                        }
                        Ok(ServerMessage::EventGap {
                            missed_from,
                            missed_to,
                        }) => {
                            tracing::warn!(
                                missed_from,
                                missed_to,
                                "daemon event stream gap; state refetch recommended"
                            );
                        }
                        Err(e) => tracing::debug!(error = %e, "ignoring malformed server message"),
                    }
                }
                // Connection ended: fail any in-flight requests.
                for (_, tx) in pending.lock().await.drain() {
                    let _ = tx.send(Err("daemon connection closed".into()));
                }
            })
        };

        Ok(Self {
            writer: Mutex::new(write_half),
            pending,
            next_id: AtomicU64::new(1),
            events,
            reader_task,
        })
    }

    /// Subscribe to the live event stream forwarded from the daemon's core.
    pub fn subscribe_events(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    /// Issue a request and await its correlated response.
    pub async fn request(&self, request: DaemonRequest) -> Result<DaemonResponse, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        if let Err(e) = {
            let mut writer = self.writer.lock().await;
            write_line(&mut *writer, &RpcRequest { id, request }).await
        } {
            self.pending.lock().await.remove(&id);
            return Err(e.into());
        }

        match rx.await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(ClientError::Server(e)),
            Err(_) => Err(ClientError::Server("response channel dropped".into())),
        }
    }

    // ---- Typed convenience helpers -------------------------------------

    pub async fn ping(&self) -> Result<(), ClientError> {
        match self.request(DaemonRequest::Ping).await? {
            DaemonResponse::Pong => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, ClientError> {
        match self.request(DaemonRequest::ListProjects).await? {
            DaemonResponse::Projects(p) => Ok(p),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn create_project(&self, input: NewProject) -> Result<Project, ClientError> {
        match self.request(DaemonRequest::CreateProject(input)).await? {
            DaemonResponse::Project(p) => Ok(p),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn list_tasks(&self, project_id: &str) -> Result<Vec<Task>, ClientError> {
        match self
            .request(DaemonRequest::ListTasks {
                project_id: project_id.to_string(),
            })
            .await?
        {
            DaemonResponse::Tasks(t) => Ok(t),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn get_work_graph(&self, project_id: &str) -> Result<WorkGraph, ClientError> {
        match self
            .request(DaemonRequest::GetWorkGraph {
                project_id: project_id.to_string(),
            })
            .await?
        {
            DaemonResponse::WorkGraph(graph) => Ok(graph),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn create_work_node(&self, input: NewWorkNode) -> Result<WorkNode, ClientError> {
        match self.request(DaemonRequest::CreateWorkNode(input)).await? {
            DaemonResponse::WorkNode(node) => Ok(node),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn update_work_node(
        &self,
        node_id: &str,
        patch: WorkNodeUpdate,
    ) -> Result<WorkNode, ClientError> {
        match self
            .request(DaemonRequest::UpdateWorkNode {
                node_id: node_id.to_string(),
                patch,
            })
            .await?
        {
            DaemonResponse::WorkNode(node) => Ok(node),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn delete_work_node(&self, node_id: &str) -> Result<(), ClientError> {
        match self
            .request(DaemonRequest::DeleteWorkNode {
                node_id: node_id.to_string(),
            })
            .await?
        {
            DaemonResponse::Unit => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn move_work_node(
        &self,
        node_id: &str,
        parent_id: Option<String>,
        position_x: f64,
        position_y: f64,
    ) -> Result<WorkNode, ClientError> {
        match self
            .request(DaemonRequest::MoveWorkNode {
                node_id: node_id.to_string(),
                parent_id,
                position_x,
                position_y,
            })
            .await?
        {
            DaemonResponse::WorkNode(node) => Ok(node),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn connect_work_nodes(&self, input: NewWorkEdge) -> Result<WorkEdge, ClientError> {
        match self.request(DaemonRequest::ConnectWorkNodes(input)).await? {
            DaemonResponse::WorkEdge(edge) => Ok(edge),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn disconnect_work_nodes(&self, edge_id: &str) -> Result<(), ClientError> {
        match self
            .request(DaemonRequest::DisconnectWorkNodes {
                edge_id: edge_id.to_string(),
            })
            .await?
        {
            DaemonResponse::Unit => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn assign_work_node_repos(
        &self,
        node_id: &str,
        repo_ids: Vec<String>,
    ) -> Result<Vec<WorkNodeRepoBinding>, ClientError> {
        match self
            .request(DaemonRequest::AssignWorkNodeRepos {
                node_id: node_id.to_string(),
                repo_ids,
            })
            .await?
        {
            DaemonResponse::WorkNodeRepoBindings(bindings) => Ok(bindings),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn run_work_node(
        &self,
        node_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
    ) -> Result<String, ClientError> {
        match self
            .request(DaemonRequest::RunWorkNode {
                node_id: node_id.to_string(),
                agent,
                permission,
                execution_backend,
            })
            .await?
        {
            DaemonResponse::SessionId(id) => Ok(id),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn stop_work_node(&self, node_id: &str) -> Result<(), ClientError> {
        match self
            .request(DaemonRequest::StopWorkNode {
                node_id: node_id.to_string(),
            })
            .await?
        {
            DaemonResponse::Unit => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn resolve_approval(
        &self,
        id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), ClientError> {
        match self
            .request(DaemonRequest::ResolveApproval {
                id: id.to_string(),
                decision,
            })
            .await?
        {
            DaemonResponse::Unit => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn list_pending_approvals(&self) -> Result<Vec<ApprovalRequest>, ClientError> {
        match self.request(DaemonRequest::ListPendingApprovals).await? {
            DaemonResponse::PendingApprovals(items) => Ok(items),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn send_work_node_message(
        &self,
        node_id: &str,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
    ) -> Result<Option<String>, ClientError> {
        match self
            .request(DaemonRequest::SendWorkNodeMessage {
                node_id: node_id.to_string(),
                agent,
                permission,
                message,
            })
            .await?
        {
            DaemonResponse::TurnIdOpt(id) => Ok(id),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn preview_context_packet(
        &self,
        node_id: &str,
    ) -> Result<ContextPacket, ClientError> {
        match self
            .request(DaemonRequest::PreviewContextPacket {
                node_id: node_id.to_string(),
            })
            .await?
        {
            DaemonResponse::ContextPacket(packet) => Ok(packet),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn work_node_diff(&self, node_id: &str) -> Result<WorkNodeDiff, ClientError> {
        match self
            .request(DaemonRequest::WorkNodeDiff {
                node_id: node_id.to_string(),
            })
            .await?
        {
            DaemonResponse::WorkNodeDiff(diff) => Ok(diff),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn get_local_model_policy(&self) -> Result<LocalModelPolicy, ClientError> {
        match self.request(DaemonRequest::GetLocalModelPolicy).await? {
            DaemonResponse::LocalModelPolicy(policy) => Ok(policy),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn set_local_model_policy(
        &self,
        policy: LocalModelPolicy,
    ) -> Result<LocalModelPolicy, ClientError> {
        match self
            .request(DaemonRequest::SetLocalModelPolicy(policy))
            .await?
        {
            DaemonResponse::LocalModelPolicy(policy) => Ok(policy),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn get_cloud_policy(&self) -> Result<CloudPolicy, ClientError> {
        match self.request(DaemonRequest::GetCloudPolicy).await? {
            DaemonResponse::CloudPolicy(policy) => Ok(policy),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn set_cloud_policy(&self, policy: CloudPolicy) -> Result<CloudPolicy, ClientError> {
        match self.request(DaemonRequest::SetCloudPolicy(policy)).await? {
            DaemonResponse::CloudPolicy(policy) => Ok(policy),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn cloud_availability(&self) -> Result<Vec<CloudAvailability>, ClientError> {
        match self.request(DaemonRequest::CloudAvailability).await? {
            DaemonResponse::CloudAvailabilities(items) => Ok(items),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn list_cloud_runs(&self, thread_id: &str) -> Result<Vec<CloudRun>, ClientError> {
        match self
            .request(DaemonRequest::ListCloudRuns {
                thread_id: thread_id.to_string(),
            })
            .await?
        {
            DaemonResponse::CloudRuns(runs) => Ok(runs),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn launch_cloud_handoff(
        &self,
        thread_id: &str,
        agent: Option<AgentKind>,
    ) -> Result<CloudRun, ClientError> {
        match self
            .request(DaemonRequest::LaunchCloudHandoff {
                thread_id: thread_id.to_string(),
                agent,
            })
            .await?
        {
            DaemonResponse::CloudRun(run) => Ok(*run),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn reclaim_cloud_run(&self, thread_id: &str) -> Result<(), ClientError> {
        match self
            .request(DaemonRequest::ReclaimCloudRun {
                thread_id: thread_id.to_string(),
            })
            .await?
        {
            DaemonResponse::Unit => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<SearchHit>, ClientError> {
        match self
            .request(DaemonRequest::Search {
                query: query.to_string(),
                project_id: project_id.map(str::to_string),
                limit,
            })
            .await?
        {
            DaemonResponse::SearchHits(h) => Ok(h),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub async fn list_activity(
        &self,
        project_id: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<ActivityEvent>, ClientError> {
        match self
            .request(DaemonRequest::ListActivity {
                project_id: project_id.map(str::to_string),
                limit,
            })
            .await?
        {
            DaemonResponse::Activity(a) => Ok(a),
            _ => Err(ClientError::Unexpected),
        }
    }
}
