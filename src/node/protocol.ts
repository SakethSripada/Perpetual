import type {
  AgentKind,
  ActivityEvent,
  AgentModelCatalog,
  AgentRunDefaults,
  AgentStatus,
  AgentThread,
  AgentThreadApplyResult,
  AgentThreadDiff,
  AgentThreadEvent,
  AgentThreadRepo,
  AgentThreadUpdate,
  AgentTurn,
  AppEvent,
  ApprovalDecision,
  ApprovalRequest,
  CloudAvailability,
  CloudPolicy,
  CloudRun,
  ClaimedCollaborationAssignment,
  CollaborationAssignment,
  CollaborationApprovalDecision,
  CollaborationChangeSet,
  CollaborationDevice,
  CollaborationEventInput,
  CollaborationSnapshot,
  ExecutionBackend,
  GithubAuthStatus,
  GithubRepository,
  LimitPolicy,
  LocalModelPolicy,
  LocalModelStatus,
  ContextPacket,
  NewAgentThread,
  NewCollaborationAssignment,
  NewGithubRepo,
  NewLocalRepo,
  NewWorkEdge,
  NewWorkNode,
  PermissionPolicy,
  Project,
  QueuedTurn,
  Repo,
  RegisterCollaborationDevice,
  SandboxLoginPrompt,
  SandboxPolicy,
  SandboxRuntimeStatus,
  WorkEdge,
  WorkGraph,
  WorkNode,
  WorkNodeDiff,
  WorkNodeRepoBinding,
  WorkNodeUpdate,
} from "./types";

export type DaemonRequest = string | Record<string, unknown>;
export type DaemonResponse = string | Record<string, unknown>;

export type ServerMessage =
  | { response: { id: number; ok?: DaemonResponse; err?: string } }
  | { event: AppEvent }
  | { event_v2: { seq: number; event: AppEvent } }
  | { event_gap: { missed_from: number; missed_to: number } };

export function variant(name: string, payload?: unknown): DaemonRequest {
  return payload === undefined ? name : { [name]: payload };
}

export function responsePayload<T = any>(response: DaemonResponse, name: string): T {
  if (typeof response === "object" && response !== null && name in response) {
    return (response as Record<string, T>)[name];
  }
  throw new Error(`Unexpected daemon response; expected ${name}`);
}

export function expectUnit(response: DaemonResponse): void {
  if (response !== "unit") {
    throw new Error("Unexpected daemon response; expected unit");
  }
}

export interface DaemonApi {
  ping(): Promise<void>;
  ensureWorkbenchProject(): Promise<Project>;
  connectLocalRepo(input: NewLocalRepo): Promise<Repo>;
  listRepos(projectId: string): Promise<Repo[]>;
  deleteRepo(repoId: string): Promise<void>;
  clearProjectRepos(projectId: string): Promise<void>;
  githubAuthStatus(token: string): Promise<GithubAuthStatus>;
  githubListRepositories(token: string): Promise<GithubRepository[]>;
  connectGithubRepo(token: string, input: NewGithubRepo): Promise<Repo>;
  detectAgents(): Promise<AgentStatus[]>;
  agentRunDefaults(): Promise<AgentRunDefaults[]>;
  agentModelCatalog(): Promise<AgentModelCatalog[]>;
  detectLocalModels(): Promise<LocalModelStatus[]>;
  getLocalModelPolicy(): Promise<LocalModelPolicy>;
  setLocalModelPolicy(policy: LocalModelPolicy): Promise<LocalModelPolicy>;
  getLimitPolicy(): Promise<LimitPolicy>;
  setLimitPolicy(policy: LimitPolicy): Promise<LimitPolicy>;
  detectSandboxRuntime(): Promise<SandboxRuntimeStatus>;
  sandboxLogin(): Promise<SandboxLoginPrompt>;
  codexSandboxLogin(): Promise<SandboxLoginPrompt>;
  getSandboxPolicy(): Promise<SandboxPolicy>;
  setSandboxPolicy(policy: SandboxPolicy): Promise<SandboxPolicy>;
  getCloudPolicy(): Promise<CloudPolicy>;
  setCloudPolicy(policy: CloudPolicy): Promise<CloudPolicy>;
  cloudAvailability(): Promise<CloudAvailability[]>;
  listCloudRuns(threadId: string): Promise<CloudRun[]>;
  launchCloudHandoff(threadId: string, agent?: AgentKind | null): Promise<CloudRun>;
  reclaimCloudRun(threadId: string): Promise<void>;
  listActivity(projectId?: string | null, limit?: number | null): Promise<ActivityEvent[]>;
  getWorkGraph(projectId: string): Promise<WorkGraph>;
  createWorkNode(input: NewWorkNode): Promise<WorkNode>;
  updateWorkNode(nodeId: string, patch: WorkNodeUpdate): Promise<WorkNode>;
  deleteWorkNode(nodeId: string): Promise<void>;
  moveWorkNode(
    nodeId: string,
    parentId: string | null,
    positionX: number,
    positionY: number
  ): Promise<WorkNode>;
  connectWorkNodes(input: NewWorkEdge): Promise<WorkEdge>;
  assignWorkNodeRepos(nodeId: string, repoIds: string[]): Promise<WorkNodeRepoBinding[]>;
  runWorkNode(
    nodeId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    executionBackend: ExecutionBackend | null
  ): Promise<string>;
  stopWorkNode(nodeId: string): Promise<void>;
  sendWorkNodeMessage(
    nodeId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    message: string
  ): Promise<string | null>;
  previewContextPacket(nodeId: string): Promise<ContextPacket>;
  workNodeDiff(nodeId: string): Promise<WorkNodeDiff>;
  listAgentThreads(projectId?: string): Promise<AgentThread[]>;
  getAgentThread(id: string): Promise<AgentThread | null>;
  createAgentThread(input: NewAgentThread): Promise<AgentThread>;
  updateAgentThread(id: string, patch: AgentThreadUpdate): Promise<AgentThread>;
  deleteAgentThread(id: string, force: boolean): Promise<void>;
  assignThreadRepos(threadId: string, repoIds: string[]): Promise<AgentThreadRepo[]>;
  listThreadRepos(threadId: string): Promise<AgentThreadRepo[]>;
  threadDiff(threadId: string): Promise<AgentThreadDiff>;
  applyThreadChanges(threadId: string): Promise<AgentThreadApplyResult>;
  runAgentThread(
    threadId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    message: string | null,
    executionBackend: ExecutionBackend | null,
    clientMessageId?: string | null
  ): Promise<string>;
  sendThreadMessage(
    threadId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    message: string,
    clientMessageId?: string | null
  ): Promise<string | null>;
  stopAgentThread(threadId: string): Promise<void>;
  listPendingApprovals(): Promise<ApprovalRequest[]>;
  resolveApproval(id: string, decision: ApprovalDecision): Promise<void>;
  listThreadEvents(threadId: string): Promise<AgentThreadEvent[]>;
  listThreadTurns(threadId: string): Promise<AgentTurn[]>;
  listQueuedTurns(threadId: string): Promise<QueuedTurn[]>;
  deleteQueuedTurn(id: string): Promise<void>;
  updateQueuedTurn(id: string, message: string): Promise<void>;
  reorderQueuedTurns(threadId: string, orderedIds: string[]): Promise<void>;
  registerCollaborationDevice(input: RegisterCollaborationDevice): Promise<CollaborationDevice>;
  heartbeatCollaborationDevice(input: RegisterCollaborationDevice): Promise<CollaborationDevice>;
  listCollaborationDevices(): Promise<CollaborationDevice[]>;
  revokeCollaborationDevice(deviceId: string): Promise<void>;
  collaborationSnapshot(
    threadId?: string | null,
    includePatches?: boolean
  ): Promise<CollaborationSnapshot>;
  createCollaborationAssignment(
    input: NewCollaborationAssignment
  ): Promise<CollaborationAssignment>;
  retryCollaborationAssignment(assignmentId: string): Promise<CollaborationAssignment>;
  listCollaborationAssignments(
    deviceId?: string | null,
    activeOnly?: boolean
  ): Promise<CollaborationAssignment[]>;
  claimCollaborationAssignment(
    assignmentId: string,
    deviceId: string
  ): Promise<ClaimedCollaborationAssignment>;
  renewCollaborationLease(
    assignmentId: string,
    leaseToken: string
  ): Promise<CollaborationAssignment>;
  reportCollaborationEvent(input: CollaborationEventInput): Promise<void>;
  reportCollaborationApproval(input: {
    assignment_id: string;
    lease_token: string;
    approval: ApprovalRequest;
  }): Promise<void>;
  listCollaborationApprovalDecisions(
    assignmentId: string,
    leaseToken: string
  ): Promise<CollaborationApprovalDecision[]>;
  acknowledgeCollaborationApprovalDecision(
    assignmentId: string,
    leaseToken: string,
    approvalId: string
  ): Promise<void>;
  reportCollaborationChangeSet(input: {
    assignment_id: string;
    lease_token: string;
    repo_id: string;
    base_ref?: string | null;
    files: import("./types").FileChange[];
    patch: string;
  }): Promise<CollaborationChangeSet>;
  finishCollaborationAssignment(input: {
    assignment_id: string;
    lease_token: string;
    state: "completed" | "interrupted" | "failed";
    error?: string | null;
  }): Promise<CollaborationAssignment>;
  cancelCollaborationAssignment(assignmentId: string): Promise<CollaborationAssignment>;
  applyCollaborationChangeSet(
    changeSetId: string,
    overwrite?: boolean
  ): Promise<CollaborationChangeSet>;
  rejectCollaborationChangeSet(changeSetId: string): Promise<CollaborationChangeSet>;
  importCollaborationPatch(threadId: string, repoId: string, patch: string): Promise<void>;
  prepareShutdown(): Promise<void>;
}
