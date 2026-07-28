//! The daemon server: hosts an [`AppCore`] and exposes it over a localhost TCP
//! socket. Each connection is authenticated with a shared token, then carries
//! request/response RPC plus a live event stream, multiplexed by a single
//! writer task.

use std::io;
use std::net::SocketAddr;

use am_core::{AppCore, CoreError};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::protocol::{
    DaemonRequest, DaemonResponse, Handshake, HandshakeAck, RpcRequest, ServerMessage,
};
use crate::write_line;

/// A bound daemon server. Call [`Server::serve`] to run the accept loop.
pub struct Server {
    core: AppCore,
    token: String,
    listener: TcpListener,
    addr: SocketAddr,
}

impl Server {
    /// Bind to `127.0.0.1` on `port` (use `0` for an OS-assigned port). The
    /// daemon is reachable only from the local machine and only by clients that
    /// present `token`.
    pub async fn bind(core: AppCore, token: String, port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let addr = listener.local_addr()?;
        Ok(Self {
            core,
            token,
            listener,
            addr,
        })
    }

    /// The address the server is listening on (includes the resolved port).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The shared auth token clients must present.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Run the accept loop. Spawns a task per connection; returns only on a
    /// fatal accept error. Abort the hosting task to stop the server.
    pub async fn serve(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, peer)) => {
                    let core = self.core.clone();
                    let token = self.token.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, core, token).await {
                            tracing::debug!(%peer, error = %e, "client connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "daemon accept failed");
                    break;
                }
            }
        }
    }
}

async fn handle_conn(stream: TcpStream, core: AppCore, token: String) -> io::Result<()> {
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    // Authenticate.
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let handshake = serde_json::from_str::<Handshake>(line.trim()).ok();
    let authed = handshake
        .as_ref()
        .map(|h| h.token == token)
        .unwrap_or(false);
    let events_v2 = handshake
        .map(|h| {
            h.capabilities
                .iter()
                .any(|c| c == crate::protocol::CAP_EVENTS_V2)
        })
        .unwrap_or(false);
    let ack = HandshakeAck {
        ok: authed,
        error: (!authed).then(|| "invalid token".to_string()),
    };
    write_line(&mut write_half, &ack).await?;
    if !authed {
        return Ok(());
    }

    // A single writer task owns the socket's write half; request replies and
    // live events are funnelled through one channel so writes never interleave.
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(256);
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_line(&mut write_half, &msg).await.is_err() {
                break;
            }
        }
    });

    // Forward core events to this client. If the broadcast channel lags, catch
    // up from the bus's replay ring so no event is silently dropped; when even
    // the ring can't cover the gap, v2 clients get an explicit EventGap frame
    // (their cue to refetch state) instead of silence.
    let mut events = core.events.subscribe();
    let event_bus = core.events.clone();
    let event_tx = tx.clone();
    let event_task = tokio::spawn(async move {
        let mut last_sent_seq: u64 = event_bus.latest_seq();
        loop {
            match events.recv().await {
                Ok(sequenced) => {
                    last_sent_seq = sequenced.seq;
                    let msg = if events_v2 {
                        ServerMessage::EventV2(sequenced)
                    } else {
                        ServerMessage::Event(sequenced.event)
                    };
                    if event_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    let replay = event_bus.replay_since(last_sent_seq);
                    if !replay.complete && events_v2 {
                        let missed_to = replay
                            .events
                            .first()
                            .map(|e| e.seq.saturating_sub(1))
                            .unwrap_or(replay.latest_seq);
                        let gap = ServerMessage::EventGap {
                            missed_from: last_sent_seq + 1,
                            missed_to,
                        };
                        if event_tx.send(gap).await.is_err() {
                            break;
                        }
                    }
                    let mut closed = false;
                    for sequenced in replay.events {
                        last_sent_seq = sequenced.seq;
                        let msg = if events_v2 {
                            ServerMessage::EventV2(sequenced)
                        } else {
                            ServerMessage::Event(sequenced.event)
                        };
                        if event_tx.send(msg).await.is_err() {
                            closed = true;
                            break;
                        }
                    }
                    if closed {
                        break;
                    }
                    // Drop the receiver's backlog; we just replayed past it.
                    events = events.resubscribe();
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    // Read + dispatch requests until the client disconnects.
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(e) => {
                tracing::debug!(error = %e, "discarding malformed request");
                continue;
            }
        };
        let core = core.clone();
        let resp_tx = tx.clone();
        // Dispatch concurrently so a slow op doesn't stall further reads.
        tokio::spawn(async move {
            let msg = match dispatch(&core, req.request).await {
                Ok(ok) => ServerMessage::Response {
                    id: req.id,
                    ok: Some(ok),
                    err: None,
                },
                Err(err) => ServerMessage::Response {
                    id: req.id,
                    ok: None,
                    err: Some(err),
                },
            };
            let _ = resp_tx.send(msg).await;
        });
    }

    event_task.abort();
    drop(tx);
    let _ = writer_task.await;
    Ok(())
}

/// Route a request to the corresponding `am-core` service method.
async fn dispatch(core: &AppCore, req: DaemonRequest) -> Result<DaemonResponse, String> {
    use DaemonRequest as Q;
    use DaemonResponse as A;
    let s = |e: CoreError| e.to_string();

    Ok(match req {
        Q::Ping => A::Pong,

        Q::ListProjects => A::Projects(core.list_projects().await.map_err(s)?),
        Q::CreateProject(input) => A::Project(core.create_project(input).await.map_err(s)?),
        Q::GetProject { id } => A::ProjectOpt(core.get_project(&id).await.map_err(s)?),
        Q::DeleteProject { id } => {
            core.delete_project(&id).await.map_err(s)?;
            A::Unit
        }
        Q::EnsureWorkbenchProject => A::Project(core.ensure_workbench_project().await.map_err(s)?),

        Q::ListTasks { project_id } => A::Tasks(core.list_tasks(&project_id).await.map_err(s)?),
        Q::CreateTask(input) => A::Task(core.create_task(input).await.map_err(s)?),
        Q::GetTask { id } => A::TaskOpt(core.get_task(&id).await.map_err(s)?),
        Q::UpdateTask { id, patch } => A::Task(core.update_task(&id, patch).await.map_err(s)?),
        Q::DeleteTask { id } => {
            core.delete_task(&id).await.map_err(s)?;
            A::Unit
        }

        Q::GetWorkGraph { project_id } => {
            A::WorkGraph(core.get_work_graph(&project_id).await.map_err(s)?)
        }
        Q::CreateWorkNode(input) => A::WorkNode(core.create_work_node(input).await.map_err(s)?),
        Q::UpdateWorkNode { node_id, patch } => {
            A::WorkNode(core.update_work_node(&node_id, patch).await.map_err(s)?)
        }
        Q::DeleteWorkNode { node_id } => {
            core.delete_work_node(&node_id).await.map_err(s)?;
            A::Unit
        }
        Q::MoveWorkNode {
            node_id,
            parent_id,
            position_x,
            position_y,
        } => A::WorkNode(
            core.move_work_node(&node_id, parent_id, position_x, position_y)
                .await
                .map_err(s)?,
        ),
        Q::ConnectWorkNodes(input) => A::WorkEdge(core.connect_work_nodes(input).await.map_err(s)?),
        Q::DisconnectWorkNodes { edge_id } => {
            core.disconnect_work_nodes(&edge_id).await.map_err(s)?;
            A::Unit
        }
        Q::AssignWorkNodeRepos { node_id, repo_ids } => A::WorkNodeRepoBindings(
            core.assign_work_node_repos(&node_id, repo_ids)
                .await
                .map_err(s)?,
        ),
        Q::RunWorkNode {
            node_id,
            agent,
            permission,
            execution_backend,
        } => A::SessionId(
            core.run_work_node(&node_id, agent, permission, execution_backend)
                .await
                .map_err(s)?,
        ),
        Q::StopWorkNode { node_id } => {
            core.stop_work_node(&node_id).await.map_err(s)?;
            A::Unit
        }
        Q::SendWorkNodeMessage {
            node_id,
            agent,
            permission,
            message,
        } => A::TurnIdOpt(
            core.send_work_node_message(&node_id, agent, permission, message)
                .await
                .map_err(s)?,
        ),
        Q::PreviewContextPacket { node_id } => {
            A::ContextPacket(core.preview_context_packet(&node_id).await.map_err(s)?)
        }
        Q::WorkNodeDiff { node_id } => {
            A::WorkNodeDiff(core.work_node_diff(&node_id).await.map_err(s)?)
        }

        Q::ConnectLocalRepo(input) => A::Repo(core.connect_local_repo(input).await.map_err(s)?),
        Q::ListRepos { project_id } => A::Repos(core.list_repos(&project_id).await.map_err(s)?),
        Q::DeleteRepo { repo_id } => {
            core.delete_repo(&repo_id).await.map_err(s)?;
            A::Unit
        }
        Q::ClearProjectRepos { project_id } => {
            core.clear_project_repos(&project_id).await.map_err(s)?;
            A::Unit
        }
        Q::GithubAuthStatus { token } => {
            A::GithubAuthStatus(core.github_auth_status_for_token(&token).await.map_err(s)?)
        }
        Q::GithubListRepositories { token } => A::GithubRepositories(
            core.github_list_repositories_with_token(&token)
                .await
                .map_err(s)?,
        ),
        Q::ConnectGithubRepo { token, input } => A::Repo(
            core.connect_github_repo_with_token(input, &token)
                .await
                .map_err(s)?,
        ),

        Q::DetectAgents => A::AgentStatuses(core.detect_agents().await.map_err(s)?),
        Q::AgentRunDefaults => A::AgentRunDefaults(core.agent_run_defaults().await.map_err(s)?),
        Q::AgentModelCatalog => A::AgentModelCatalogs(core.agent_model_catalog().await.map_err(s)?),
        Q::DetectLocalModels => A::LocalModelStatuses(core.detect_local_models().await.map_err(s)?),
        Q::GetLocalModelPolicy => {
            A::LocalModelPolicy(core.get_local_model_policy().await.map_err(s)?)
        }
        Q::SetLocalModelPolicy(policy) => {
            A::LocalModelPolicy(core.set_local_model_policy(policy).await.map_err(s)?)
        }
        Q::GetLimitPolicy => A::LimitPolicy(core.get_limit_policy().await.map_err(s)?),
        Q::SetLimitPolicy(policy) => {
            A::LimitPolicy(core.set_limit_policy(policy).await.map_err(s)?)
        }
        Q::DetectSandboxRuntime => {
            A::SandboxRuntimeStatus(core.detect_sandbox_runtime().await.map_err(s)?)
        }
        Q::SandboxLogin => A::SandboxLoginPrompt(core.sandbox_login().await.map_err(s)?),
        Q::CodexSandboxLogin => A::SandboxLoginPrompt(core.codex_sandbox_login().await.map_err(s)?),
        Q::GetSandboxPolicy => A::SandboxPolicy(core.get_sandbox_policy().await.map_err(s)?),
        Q::SetSandboxPolicy(policy) => {
            A::SandboxPolicy(core.set_sandbox_policy(policy).await.map_err(s)?)
        }
        Q::GetCloudPolicy => A::CloudPolicy(core.get_cloud_policy().await.map_err(s)?),
        Q::SetCloudPolicy(policy) => {
            A::CloudPolicy(core.set_cloud_policy(policy).await.map_err(s)?)
        }
        Q::CloudAvailability => A::CloudAvailabilities(core.cloud_availability().await.map_err(s)?),
        Q::ListCloudRuns { thread_id } => {
            A::CloudRuns(core.list_cloud_runs(&thread_id).await.map_err(s)?)
        }
        Q::LaunchCloudHandoff { thread_id, agent } => A::CloudRun(Box::new(
            core.start_thread_cloud_handoff(
                &thread_id,
                am_proto::CloudHandoffTrigger::Manual,
                agent,
            )
            .await
            .map_err(s)?,
        )),
        Q::ReclaimCloudRun { thread_id } => {
            core.reclaim_cloud_run(&thread_id).await.map_err(s)?;
            A::Unit
        }

        Q::ListKnowledgeDocs { project_id } => {
            A::KnowledgeDocs(core.list_knowledge_docs(&project_id).await.map_err(s)?)
        }
        Q::CreateKnowledgeDoc(input) => {
            A::KnowledgeDoc(core.create_knowledge_doc(input).await.map_err(s)?)
        }
        Q::UpdateKnowledgeDoc { id, patch } => {
            A::KnowledgeDoc(core.update_knowledge_doc(&id, patch).await.map_err(s)?)
        }
        Q::DeleteKnowledgeDoc { id } => {
            core.delete_knowledge_doc(&id).await.map_err(s)?;
            A::Unit
        }

        Q::ListProjectMemory { project_id } => {
            A::MemoryNotes(core.list_project_memory(&project_id).await.map_err(s)?)
        }
        Q::ListTaskMemory { task_id } => {
            A::MemoryNotes(core.list_task_memory(&task_id).await.map_err(s)?)
        }
        Q::CreateMemoryNote(input) => {
            A::MemoryNote(core.create_memory_note(input).await.map_err(s)?)
        }
        Q::UpdateMemoryNote { id, patch } => {
            A::MemoryNote(core.update_memory_note(&id, patch).await.map_err(s)?)
        }
        Q::DeleteMemoryNote { id } => {
            core.delete_memory_note(&id).await.map_err(s)?;
            A::Unit
        }

        Q::Search {
            query,
            project_id,
            limit,
        } => A::SearchHits(
            core.search(&query, project_id.as_deref(), limit.unwrap_or(50))
                .await
                .map_err(s)?,
        ),
        Q::ListActivity { project_id, limit } => A::Activity(
            core.list_activity(project_id.as_deref(), limit.unwrap_or(100))
                .await
                .map_err(s)?,
        ),

        Q::RunTask {
            task_id,
            agent,
            permission,
        } => A::SessionId(
            core.run_task(&task_id, agent, permission)
                .await
                .map_err(s)?,
        ),
        Q::StopTask { task_id } => {
            core.stop_task(&task_id).await.map_err(s)?;
            A::Unit
        }
        Q::TaskDiff { task_id } => A::Diff(core.task_diff(&task_id).await.map_err(s)?),

        Q::ResolveApproval { id, decision } => {
            core.resolve_approval(&id, decision).await.map_err(s)?;
            A::Unit
        }
        Q::ListPendingApprovals => A::PendingApprovals(core.list_pending_approvals().await),

        Q::ListAgentThreads { project_id } => A::AgentThreads(
            core.list_agent_threads(project_id.as_deref())
                .await
                .map_err(s)?,
        ),
        Q::CreateAgentThread(input) => {
            A::AgentThread(core.create_agent_thread(input).await.map_err(s)?)
        }
        Q::GetAgentThread { id } => A::AgentThreadOpt(core.get_agent_thread(&id).await.map_err(s)?),
        Q::UpdateAgentThread { id, patch } => {
            A::AgentThread(core.update_agent_thread(&id, patch).await.map_err(s)?)
        }
        Q::DeleteAgentThread { id, force } => {
            core.delete_agent_thread(&id, force).await.map_err(s)?;
            A::Unit
        }
        Q::AssignThreadRepos {
            thread_id,
            repo_ids,
        } => A::AgentThreadRepos(
            core.assign_thread_repos(&thread_id, repo_ids)
                .await
                .map_err(s)?,
        ),
        Q::ListThreadRepos { thread_id } => {
            A::AgentThreadRepos(core.list_thread_repos(&thread_id).await.map_err(s)?)
        }
        Q::ThreadDiff { thread_id } => {
            A::AgentThreadDiff(core.thread_diff(&thread_id).await.map_err(s)?)
        }
        Q::ApplyThreadChanges { thread_id } => {
            A::AgentThreadApplyResult(core.apply_thread_changes(&thread_id).await.map_err(s)?)
        }
        Q::RunAgentThread {
            thread_id,
            agent,
            permission,
            message,
            execution_backend,
            client_message_id,
        } => A::TurnId(
            core.run_agent_thread_with_client_message(
                &thread_id,
                agent,
                permission,
                message,
                execution_backend,
                client_message_id,
            )
            .await
            .map_err(s)?,
        ),
        Q::SendThreadMessage {
            thread_id,
            agent,
            permission,
            message,
            client_message_id,
        } => A::TurnIdOpt(
            core.send_thread_message(&thread_id, agent, permission, message, client_message_id)
                .await
                .map_err(s)?,
        ),
        Q::StopAgentThread { thread_id } => {
            core.stop_agent_thread(&thread_id).await.map_err(s)?;
            A::Unit
        }
        Q::ListThreadEvents { thread_id } => {
            A::AgentThreadEvents(core.list_thread_events(&thread_id).await.map_err(s)?)
        }
        Q::ListThreadTurns { thread_id } => {
            A::AgentTurns(core.list_thread_turns(&thread_id).await.map_err(s)?)
        }
        Q::ListQueuedTurns { thread_id } => {
            A::QueuedTurns(core.list_queued_turns(&thread_id).await.map_err(s)?)
        }
        Q::DeleteQueuedTurn { id } => {
            core.delete_queued_turn(&id).await.map_err(s)?;
            A::Unit
        }
        Q::UpdateQueuedTurn { id, message } => {
            core.update_queued_turn(&id, &message).await.map_err(s)?;
            A::Unit
        }
        Q::ReorderQueuedTurns {
            thread_id,
            ordered_ids,
        } => {
            core.reorder_queued_turns(&thread_id, ordered_ids)
                .await
                .map_err(s)?;
            A::Unit
        }

        Q::RegisterCollaborationDevice(input) => {
            A::CollaborationDevice(core.register_collaboration_device(input).await.map_err(s)?)
        }
        Q::HeartbeatCollaborationDevice(input) => A::CollaborationDevice(
            core.heartbeat_collaboration_device(input)
                .await
                .map_err(s)?,
        ),
        Q::ListCollaborationDevices => {
            A::CollaborationDevices(core.list_collaboration_devices().await.map_err(s)?)
        }
        Q::RevokeCollaborationDevice { device_id } => {
            core.revoke_collaboration_device(&device_id)
                .await
                .map_err(s)?;
            A::Unit
        }
        Q::CollaborationSnapshot {
            thread_id,
            include_patches,
        } => A::CollaborationSnapshot(
            core.collaboration_snapshot(thread_id.as_deref(), include_patches)
                .await
                .map_err(s)?,
        ),
        Q::CreateCollaborationAssignment(input) => A::CollaborationAssignment(
            core.create_collaboration_assignment(input)
                .await
                .map_err(s)?,
        ),
        Q::ListCollaborationAssignments {
            device_id,
            active_only,
        } => A::CollaborationAssignments(
            core.list_collaboration_assignments(device_id.as_deref(), active_only)
                .await
                .map_err(s)?,
        ),
        Q::ClaimCollaborationAssignment {
            assignment_id,
            device_id,
        } => A::ClaimedCollaborationAssignment(
            core.claim_collaboration_assignment(&assignment_id, &device_id)
                .await
                .map_err(s)?,
        ),
        Q::RenewCollaborationLease {
            assignment_id,
            lease_token,
        } => A::CollaborationAssignment(
            core.renew_collaboration_lease(&assignment_id, &lease_token)
                .await
                .map_err(s)?,
        ),
        Q::ReportCollaborationEvent(input) => {
            core.report_collaboration_event(input).await.map_err(s)?;
            A::Unit
        }
        Q::ReportCollaborationApproval(input) => {
            core.report_collaboration_approval(input).await.map_err(s)?;
            A::Unit
        }
        Q::ListCollaborationApprovalDecisions {
            assignment_id,
            lease_token,
        } => A::CollaborationApprovalDecisions(
            core.list_collaboration_approval_decisions(&assignment_id, &lease_token)
                .await
                .map_err(s)?,
        ),
        Q::AcknowledgeCollaborationApprovalDecision {
            assignment_id,
            lease_token,
            approval_id,
        } => {
            core.acknowledge_collaboration_approval_decision(
                &assignment_id,
                &lease_token,
                &approval_id,
            )
            .await
            .map_err(s)?;
            A::Unit
        }
        Q::ReportCollaborationChangeSet(input) => A::CollaborationChangeSet(
            core.report_collaboration_change_set(input)
                .await
                .map_err(s)?,
        ),
        Q::FinishCollaborationAssignment(input) => A::CollaborationAssignment(
            core.finish_collaboration_assignment(input)
                .await
                .map_err(s)?,
        ),
        Q::CancelCollaborationAssignment { assignment_id } => A::CollaborationAssignment(
            core.cancel_collaboration_assignment(&assignment_id)
                .await
                .map_err(s)?,
        ),
        Q::ApplyCollaborationChangeSet {
            change_set_id,
            overwrite,
        } => A::CollaborationChangeSet(
            core.apply_collaboration_change_set(&change_set_id, overwrite)
                .await
                .map_err(s)?,
        ),
        Q::RejectCollaborationChangeSet { change_set_id } => A::CollaborationChangeSet(
            core.reject_collaboration_change_set(&change_set_id)
                .await
                .map_err(s)?,
        ),
        Q::ImportCollaborationPatch {
            thread_id,
            repo_id,
            patch,
        } => {
            core.import_collaboration_patch(&thread_id, &repo_id, &patch)
                .await
                .map_err(s)?;
            A::Unit
        }

        Q::ReplayEvents { since_seq } => A::EventReplay(core.events.replay_since(since_seq)),
        Q::LatestEventSeq => A::EventSeq(core.events.latest_seq()),
        Q::PrepareShutdown => {
            core.prepare_shutdown().await.map_err(s)?;
            A::Unit
        }
    })
}
