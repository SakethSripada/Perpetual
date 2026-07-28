//! Wire protocol between the daemon and its clients.
//!
//! Framing is newline-delimited JSON (one JSON value per line; `serde_json`
//! never emits interior newlines, so lines are safe frames). A client first
//! sends a [`Handshake`] line carrying the shared token; the server replies
//! with a [`HandshakeAck`]. After that, the client sends [`RpcRequest`] lines
//! and the server sends [`ServerMessage`] lines (responses and live events,
//! multiplexed on one connection).

use am_agents::PermissionPolicy;
use am_proto::{
    ActivityEvent, AgentKind, AgentModelCatalog, AgentRunDefaults, AgentStatus, AgentThread,
    AgentThreadApplyResult, AgentThreadDiff, AgentThreadEvent, AgentThreadRepo, AgentThreadUpdate,
    AgentTurn, AppEvent, ApprovalDecision, ApprovalRequest, ClaimedCollaborationAssignment,
    CloudAvailability, CloudPolicy, CloudRun, CollaborationAssignment, CollaborationChangeSet,
    CollaborationDevice, CollaborationEventInput, CollaborationSnapshot, ContextPacket,
    EventReplay, ExecutionBackend, FinishCollaborationAssignment, GithubAuthStatus,
    GithubRepository, KnowledgeDoc, KnowledgeDocUpdate, LimitPolicy, LocalModelPolicy,
    LocalModelStatus, MemoryNote, MemoryNoteUpdate, NewAgentThread, NewCollaborationAssignment,
    NewCollaborationChangeSet, NewGithubRepo, NewKnowledgeDoc, NewLocalRepo, NewMemoryNote,
    NewProject, NewTask, NewWorkEdge, NewWorkNode, Project, QueuedTurn,
    RegisterCollaborationDevice, Repo, SandboxLoginPrompt, SandboxPolicy, SandboxRuntimeStatus,
    SearchHit, SequencedEvent, Task, TaskDiff, TaskUpdate, WorkEdge, WorkGraph, WorkNode,
    WorkNodeDiff, WorkNodeRepoBinding, WorkNodeUpdate,
};
use serde::{Deserialize, Serialize};

/// First line a client sends: authenticates the connection.
///
/// `capabilities` is additive and defaults to empty, so older clients (and the
/// VS Code extension's synced daemon) remain wire-compatible. Advertising
/// [`CAP_EVENTS_V2`] opts in to sequenced [`ServerMessage::EventV2`] frames
/// (with lag replay) instead of legacy [`ServerMessage::Event`] frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handshake {
    pub token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Capability string: client understands `EventV2`/`EventGap` frames.
pub const CAP_EVENTS_V2: &str = "events_v2";

/// Server's reply to a [`Handshake`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeAck {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// A client request envelope. `id` correlates the eventual [`ServerMessage::Response`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub request: DaemonRequest,
}

/// Everything a client can ask the daemon to do. Mirrors `am-core`'s service
/// surface so a UI can drive the core entirely over the socket.
///
/// Externally tagged (the serde default): the representation handles every
/// variant shape, including newtype variants wrapping sequences — internal
/// tagging cannot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonRequest {
    Ping,

    // Projects
    ListProjects,
    CreateProject(NewProject),
    GetProject {
        id: String,
    },
    DeleteProject {
        id: String,
    },

    // Tasks
    ListTasks {
        project_id: String,
    },
    CreateTask(NewTask),
    GetTask {
        id: String,
    },
    UpdateTask {
        id: String,
        patch: TaskUpdate,
    },
    DeleteTask {
        id: String,
    },

    // Work graph
    GetWorkGraph {
        project_id: String,
    },
    CreateWorkNode(NewWorkNode),
    UpdateWorkNode {
        node_id: String,
        patch: WorkNodeUpdate,
    },
    DeleteWorkNode {
        node_id: String,
    },
    MoveWorkNode {
        node_id: String,
        parent_id: Option<String>,
        position_x: f64,
        position_y: f64,
    },
    ConnectWorkNodes(NewWorkEdge),
    DisconnectWorkNodes {
        edge_id: String,
    },
    AssignWorkNodeRepos {
        node_id: String,
        repo_ids: Vec<String>,
    },
    RunWorkNode {
        node_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
        execution_backend: Option<ExecutionBackend>,
    },
    StopWorkNode {
        node_id: String,
    },
    SendWorkNodeMessage {
        node_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
    },
    PreviewContextPacket {
        node_id: String,
    },
    WorkNodeDiff {
        node_id: String,
    },

    // Repos
    ConnectLocalRepo(NewLocalRepo),
    ListRepos {
        project_id: String,
    },
    DeleteRepo {
        repo_id: String,
    },
    ClearProjectRepos {
        project_id: String,
    },
    GithubAuthStatus {
        token: String,
    },
    GithubListRepositories {
        token: String,
    },
    ConnectGithubRepo {
        token: String,
        input: NewGithubRepo,
    },

    // Agent readiness / settings
    DetectAgents,
    AgentRunDefaults,
    AgentModelCatalog,
    DetectLocalModels,
    GetLocalModelPolicy,
    SetLocalModelPolicy(LocalModelPolicy),
    GetLimitPolicy,
    SetLimitPolicy(LimitPolicy),
    DetectSandboxRuntime,
    SandboxLogin,
    CodexSandboxLogin,
    GetSandboxPolicy,
    SetSandboxPolicy(SandboxPolicy),
    GetCloudPolicy,
    SetCloudPolicy(CloudPolicy),
    CloudAvailability,
    ListCloudRuns {
        thread_id: String,
    },
    LaunchCloudHandoff {
        thread_id: String,
        agent: Option<AgentKind>,
    },
    ReclaimCloudRun {
        thread_id: String,
    },

    // Knowledge
    ListKnowledgeDocs {
        project_id: String,
    },
    CreateKnowledgeDoc(NewKnowledgeDoc),
    UpdateKnowledgeDoc {
        id: String,
        patch: KnowledgeDocUpdate,
    },
    DeleteKnowledgeDoc {
        id: String,
    },

    // Memory
    ListProjectMemory {
        project_id: String,
    },
    ListTaskMemory {
        task_id: String,
    },
    CreateMemoryNote(NewMemoryNote),
    UpdateMemoryNote {
        id: String,
        patch: MemoryNoteUpdate,
    },
    DeleteMemoryNote {
        id: String,
    },

    // Search & activity
    Search {
        query: String,
        project_id: Option<String>,
        limit: Option<i64>,
    },
    ListActivity {
        project_id: Option<String>,
        limit: Option<i64>,
    },

    // Execution
    RunTask {
        task_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
    },
    StopTask {
        task_id: String,
    },
    TaskDiff {
        task_id: String,
    },

    // Approvals
    ResolveApproval {
        id: String,
        decision: ApprovalDecision,
    },
    ListPendingApprovals,

    // Agent threads / Workbench
    EnsureWorkbenchProject,
    ListAgentThreads {
        project_id: Option<String>,
    },
    CreateAgentThread(NewAgentThread),
    GetAgentThread {
        id: String,
    },
    UpdateAgentThread {
        id: String,
        patch: AgentThreadUpdate,
    },
    DeleteAgentThread {
        id: String,
        force: bool,
    },
    AssignThreadRepos {
        thread_id: String,
        repo_ids: Vec<String>,
    },
    ListThreadRepos {
        thread_id: String,
    },
    ThreadDiff {
        thread_id: String,
    },
    ApplyThreadChanges {
        thread_id: String,
    },
    RunAgentThread {
        thread_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: Option<String>,
        execution_backend: Option<ExecutionBackend>,
        #[serde(default)]
        client_message_id: Option<String>,
    },
    SendThreadMessage {
        thread_id: String,
        agent: AgentKind,
        permission: PermissionPolicy,
        message: String,
        #[serde(default)]
        client_message_id: Option<String>,
    },
    StopAgentThread {
        thread_id: String,
    },
    ListThreadEvents {
        thread_id: String,
    },
    ListThreadTurns {
        thread_id: String,
    },
    ListQueuedTurns {
        thread_id: String,
    },
    DeleteQueuedTurn {
        id: String,
    },
    UpdateQueuedTurn {
        id: String,
        message: String,
    },
    ReorderQueuedTurns {
        thread_id: String,
        ordered_ids: Vec<String>,
    },

    // Multi-device collaboration
    RegisterCollaborationDevice(RegisterCollaborationDevice),
    HeartbeatCollaborationDevice(RegisterCollaborationDevice),
    ListCollaborationDevices,
    RevokeCollaborationDevice {
        device_id: String,
    },
    CollaborationSnapshot {
        thread_id: Option<String>,
    },
    CreateCollaborationAssignment(NewCollaborationAssignment),
    ListCollaborationAssignments {
        device_id: Option<String>,
        #[serde(default)]
        active_only: bool,
    },
    ClaimCollaborationAssignment {
        assignment_id: String,
        device_id: String,
    },
    RenewCollaborationLease {
        assignment_id: String,
        lease_token: String,
    },
    ReportCollaborationEvent(CollaborationEventInput),
    ReportCollaborationChangeSet(NewCollaborationChangeSet),
    FinishCollaborationAssignment(FinishCollaborationAssignment),
    CancelCollaborationAssignment {
        assignment_id: String,
    },

    // Event stream recovery (additive; requires no capability)
    ReplayEvents {
        since_seq: u64,
    },
    LatestEventSeq,
    PrepareShutdown,
}

/// Successful result payloads, one variant per request shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonResponse {
    Pong,
    Unit,
    Project(Project),
    ProjectOpt(Option<Project>),
    Projects(Vec<Project>),
    Task(Task),
    TaskOpt(Option<Task>),
    Tasks(Vec<Task>),
    WorkGraph(WorkGraph),
    WorkNode(WorkNode),
    WorkEdge(WorkEdge),
    WorkNodeRepoBindings(Vec<WorkNodeRepoBinding>),
    ContextPacket(ContextPacket),
    WorkNodeDiff(WorkNodeDiff),
    AgentStatuses(Vec<AgentStatus>),
    AgentRunDefaults(Vec<AgentRunDefaults>),
    AgentModelCatalogs(Vec<AgentModelCatalog>),
    LocalModelStatuses(Vec<LocalModelStatus>),
    LocalModelPolicy(LocalModelPolicy),
    LimitPolicy(LimitPolicy),
    SandboxRuntimeStatus(SandboxRuntimeStatus),
    SandboxLoginPrompt(SandboxLoginPrompt),
    SandboxPolicy(SandboxPolicy),
    CloudPolicy(CloudPolicy),
    CloudAvailabilities(Vec<CloudAvailability>),
    CloudRun(Box<CloudRun>),
    CloudRuns(Vec<CloudRun>),
    GithubAuthStatus(GithubAuthStatus),
    GithubRepositories(Vec<GithubRepository>),
    Repo(Repo),
    Repos(Vec<Repo>),
    KnowledgeDoc(KnowledgeDoc),
    KnowledgeDocs(Vec<KnowledgeDoc>),
    MemoryNote(MemoryNote),
    MemoryNotes(Vec<MemoryNote>),
    SearchHits(Vec<SearchHit>),
    Activity(Vec<ActivityEvent>),
    SessionId(String),
    Diff(TaskDiff),
    PendingApprovals(Vec<ApprovalRequest>),
    AgentThread(AgentThread),
    AgentThreadOpt(Option<AgentThread>),
    AgentThreads(Vec<AgentThread>),
    AgentThreadRepos(Vec<AgentThreadRepo>),
    AgentThreadDiff(AgentThreadDiff),
    AgentThreadApplyResult(AgentThreadApplyResult),
    AgentThreadEvents(Vec<AgentThreadEvent>),
    AgentTurns(Vec<AgentTurn>),
    QueuedTurns(Vec<QueuedTurn>),
    TurnId(String),
    TurnIdOpt(Option<String>),
    CollaborationDevice(CollaborationDevice),
    CollaborationDevices(Vec<CollaborationDevice>),
    CollaborationAssignment(CollaborationAssignment),
    CollaborationAssignments(Vec<CollaborationAssignment>),
    ClaimedCollaborationAssignment(ClaimedCollaborationAssignment),
    CollaborationChangeSet(CollaborationChangeSet),
    CollaborationSnapshot(CollaborationSnapshot),
    EventReplay(EventReplay),
    EventSeq(u64),
}

/// Anything the server sends after the handshake: a response to a request, or a
/// live event broadcast from the core's event bus. Externally tagged so it does
/// not collide with [`AppEvent`]'s own internal `type` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    Response {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<DaemonResponse>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<String>,
    },
    Event(AppEvent),
    /// Sequenced live event; sent instead of `Event` to clients that
    /// advertised [`CAP_EVENTS_V2`] in their handshake.
    EventV2(SequencedEvent),
    /// The live stream lagged beyond the replay ring; events in
    /// `missed_from..=missed_to` were dropped. Only sent to
    /// [`CAP_EVENTS_V2`] clients, which should refetch state.
    EventGap {
        missed_from: u64,
        missed_to: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::{now, Project, TaskPriority, TaskStatus};

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        // Exactly the framing the transport uses: serialize to a single line,
        // then parse it back.
        let line = serde_json::to_string(value).expect("serialize");
        assert!(!line.contains('\n'), "frames must be single-line");
        serde_json::from_str(&line).expect("deserialize")
    }

    fn project() -> Project {
        Project {
            id: "p1".into(),
            name: "Demo".into(),
            description: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn response_with_sequence_variant_roundtrips() {
        // Regression: internal tagging could not serialize a newtype variant
        // wrapping a Vec. External tagging must.
        let msg = ServerMessage::Response {
            id: 7,
            ok: Some(DaemonResponse::Projects(vec![project()])),
            err: None,
        };
        match roundtrip(&msg) {
            ServerMessage::Response {
                id,
                ok: Some(DaemonResponse::Projects(ps)),
                err: None,
            } => {
                assert_eq!(id, 7);
                assert_eq!(ps.len(), 1);
                assert_eq!(ps[0].id, "p1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn error_response_roundtrips() {
        let msg = ServerMessage::Response {
            id: 3,
            ok: None,
            err: Some("boom".into()),
        };
        match roundtrip(&msg) {
            ServerMessage::Response {
                id: 3,
                ok: None,
                err: Some(e),
            } => assert_eq!(e, "boom"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn event_does_not_collide_with_appevent_type_tag() {
        // AppEvent is itself tagged on "type"; ServerMessage must wrap it without
        // clobbering that tag.
        let task = am_proto::Task {
            id: "t1".into(),
            project_id: "p1".into(),
            title: "x".into(),
            description: None,
            status: TaskStatus::Running,
            priority: TaskPriority::Medium,
            primary_agent: None,
            model: None,
            model_target: am_proto::ModelTargetKind::FrontierDefault,
            compute_lease_id: None,
            compute_provider: None,
            estimated_compute_cost_usd: None,
            fallback_model_target: None,
            created_at: now(),
            updated_at: now(),
        };
        let msg = ServerMessage::Event(AppEvent::TaskUpdated(task));
        match roundtrip(&msg) {
            ServerMessage::Event(AppEvent::TaskUpdated(t)) => assert_eq!(t.id, "t1"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn request_envelope_roundtrips() {
        let req = RpcRequest {
            id: 42,
            request: DaemonRequest::Search {
                query: "auth".into(),
                project_id: Some("p1".into()),
                limit: Some(20),
            },
        };
        let back = roundtrip(&req);
        assert_eq!(back.id, 42);
        assert!(matches!(back.request, DaemonRequest::Search { .. }));
    }

    #[test]
    fn local_model_policy_request_response_roundtrips() {
        let policy = am_proto::LocalModelPolicy::default();
        let req = RpcRequest {
            id: 55,
            request: DaemonRequest::SetLocalModelPolicy(policy.clone()),
        };
        match roundtrip(&req).request {
            DaemonRequest::SetLocalModelPolicy(back) => {
                assert_eq!(back.use_local_fallback, policy.use_local_fallback);
                assert_eq!(back.switch_back_to_cloud, policy.switch_back_to_cloud);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let msg = ServerMessage::Response {
            id: 55,
            ok: Some(DaemonResponse::LocalModelPolicy(policy)),
            err: None,
        };
        assert!(matches!(
            roundtrip(&msg),
            ServerMessage::Response {
                ok: Some(DaemonResponse::LocalModelPolicy(_)),
                ..
            }
        ));
    }

    #[test]
    fn legacy_handshake_without_capabilities_still_parses() {
        // Wire compatibility with pre-capability clients (VS Code extension's
        // synced daemon): a bare token frame must keep deserializing.
        let legacy = r#"{"token":"secret"}"#;
        let parsed: Handshake = serde_json::from_str(legacy).expect("legacy handshake");
        assert_eq!(parsed.token, "secret");
        assert!(parsed.capabilities.is_empty());

        // And a v2 handshake roundtrips with its capability intact.
        let v2 = roundtrip(&Handshake {
            token: "secret".into(),
            capabilities: vec![CAP_EVENTS_V2.into()],
        });
        assert_eq!(v2.capabilities, vec![CAP_EVENTS_V2.to_string()]);
    }

    #[test]
    fn sequenced_event_frames_roundtrip() {
        let event = AppEvent::WorkGraphUpdated {
            project_id: "p1".into(),
        };
        let msg = ServerMessage::EventV2(am_proto::SequencedEvent { seq: 41, event });
        match roundtrip(&msg) {
            ServerMessage::EventV2(sequenced) => {
                assert_eq!(sequenced.seq, 41);
                assert!(matches!(sequenced.event, AppEvent::WorkGraphUpdated { .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }

        match roundtrip(&ServerMessage::EventGap {
            missed_from: 10,
            missed_to: 12,
        }) {
            ServerMessage::EventGap {
                missed_from: 10,
                missed_to: 12,
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn replay_request_and_response_roundtrip() {
        let req = roundtrip(&RpcRequest {
            id: 9,
            request: DaemonRequest::ReplayEvents { since_seq: 100 },
        });
        assert!(matches!(
            req.request,
            DaemonRequest::ReplayEvents { since_seq: 100 }
        ));

        let msg = ServerMessage::Response {
            id: 9,
            ok: Some(DaemonResponse::EventReplay(EventReplay {
                complete: true,
                latest_seq: 101,
                events: vec![am_proto::SequencedEvent {
                    seq: 101,
                    event: AppEvent::WorkGraphUpdated {
                        project_id: "p1".into(),
                    },
                }],
            })),
            err: None,
        };
        match roundtrip(&msg) {
            ServerMessage::Response {
                ok: Some(DaemonResponse::EventReplay(replay)),
                ..
            } => {
                assert!(replay.complete);
                assert_eq!(replay.latest_seq, 101);
                assert_eq!(replay.events.len(), 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
