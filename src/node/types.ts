export type AgentKind = "claude_code" | "codex" | "gemini" | "cursor" | "open_code";
export type TaskStatus =
  | "draft"
  | "queued"
  | "running"
  | "awaiting_approval"
  | "waiting_for_limit"
  | "waiting_for_network"
  | "paused"
  | "review"
  | "done"
  | "failed"
  | "cancelled";
export type TaskPriority = "low" | "medium" | "high" | "urgent";
export type PermissionPolicy = "read_only" | "workspace_write" | "autonomous";
export type ExecutionBackend = "host" | "docker_sandbox";
export type AvailabilityState = "unknown" | "available" | "limited";

export interface Project {
  id: string;
  name: string;
  description: string | null;
  created_at: string;
  updated_at: string;
}

export interface Repo {
  id: string;
  project_id: string;
  name: string;
  kind: "local" | "github";
  local_path: string | null;
  remote_url: string | null;
  default_branch: string;
  created_at: string;
  updated_at: string;
}

export interface NewLocalRepo {
  project_id: string;
  path: string;
}

export interface NewGithubRepo {
  project_id: string;
  name: string;
  full_name: string;
  clone_url: string;
  ssh_url: string;
  default_branch: string;
}

export interface GithubAuthStatus {
  configured: boolean;
  authenticated: boolean;
  login: string | null;
  avatar_url: string | null;
  error: string | null;
}

export interface GithubRepository {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  html_url: string;
  clone_url: string;
  ssh_url: string;
  default_branch: string;
  updated_at: string | null;
}

export interface AgentStatus {
  kind: AgentKind;
  installed: boolean;
  authenticated: boolean;
  version: string | null;
  binary_path: string | null;
  availability: AvailabilityState;
  reset_at: string | null;
  last_checked: string | null;
}

export interface AgentRunDefaults {
  kind: AgentKind;
  model: string | null;
  reasoning: string | null;
}

export interface LimitPolicy {
  auto_switch: boolean;
  switch_back: boolean;
  agent_priority: AgentKind[];
  resume_with_earliest: boolean;
  unknown_reset_retry_secs: number;
  keep_awake: boolean;
}

export interface SandboxPolicy {
  default_backend: ExecutionBackend;
  max_concurrent_sandboxes: number;
  cpus: number;
  memory: string;
  network_preset: string;
  run_timeout_secs: number;
  idle_timeout_secs: number;
  stop_grace_secs: number;
}

export interface SandboxRuntimeStatus {
  installed: boolean;
  authenticated: boolean;
  codex_authenticated: boolean;
  version: string | null;
  binary_path: string | null;
  active_count: number;
  error: string | null;
  codex_error: string | null;
}

export interface SandboxLoginPrompt {
  code: string;
  url: string;
}

export interface AgentThread {
  id: string;
  project_id: string | null;
  title: string;
  status: TaskStatus;
  active_agent: AgentKind | null;
  preferred_agent: AgentKind | null;
  permission: PermissionPolicy;
  execution_backend: ExecutionBackend;
  model: string | null;
  reasoning: string | null;
  original_agent: AgentKind | null;
  fallback_agent: AgentKind | null;
  limit_reset_at: string | null;
  switch_back: boolean;
  handoff_state: string;
  objective: string;
  decisions: string;
  progress: string;
  open_questions: string;
  next_actions: string;
  created_at: string;
  updated_at: string;
}

export interface NewAgentThread {
  project_id?: string | null;
  title: string;
  objective?: string | null;
  repo_ids?: string[];
  preferred_agent?: AgentKind | null;
  permission?: PermissionPolicy | null;
  execution_backend?: ExecutionBackend | null;
  model?: string | null;
  reasoning?: string | null;
}

export interface AgentThreadUpdate {
  title?: string | null;
  status?: TaskStatus | null;
  active_agent?: AgentKind | null;
  preferred_agent?: AgentKind | null;
  permission?: PermissionPolicy | null;
  execution_backend?: ExecutionBackend | null;
  model?: string | null;
  reasoning?: string | null;
  objective?: string | null;
  decisions?: string | null;
  progress?: string | null;
  open_questions?: string | null;
  next_actions?: string | null;
}

export interface AgentThreadRepo {
  thread_id: string;
  repo_id: string;
  repo_name: string;
  worktree_path: string | null;
  branch: string | null;
  base_ref: string | null;
  workspace_backend: ExecutionBackend;
}

export interface AgentThreadEvent {
  id: string;
  thread_id: string;
  turn_id: string;
  role: string;
  kind: string;
  text: string | null;
  data: unknown;
  ts: string;
}

export interface AgentTurn {
  id: string;
  thread_id: string;
  agent_kind: AgentKind;
  agent_session_id: string | null;
  state: "running" | "completed" | "interrupted" | "failed";
  permission: PermissionPolicy;
  execution_backend: ExecutionBackend;
  sandbox_name: string | null;
  started_at: string;
  ended_at: string | null;
}

export interface QueuedTurn {
  id: string;
  thread_id: string;
  agent_kind: AgentKind;
  permission: PermissionPolicy;
  message: string;
  created_at: string;
}

export interface FileChange {
  path: string;
  status: string;
  additions: number;
  deletions: number;
}

export interface AgentThreadRepoDiff {
  repo_id: string;
  repo_name: string;
  remote_url: string | null;
  branch: string | null;
  base_ref: string | null;
  head_ref: string | null;
  worktree_path: string | null;
  files: FileChange[];
  patch: string;
}

export interface AgentThreadDiff {
  repos: AgentThreadRepoDiff[];
}

export interface TaskDiff {
  files: FileChange[];
  patch: string;
  repo_id: string | null;
  repo_name: string | null;
  remote_url: string | null;
  branch: string | null;
  base_ref: string | null;
  head_ref: string | null;
  worktree_path: string | null;
}

export type WorkNodeKind = "group" | "task" | "session" | "milestone";
export type WorkEdgeKind =
  | "depends_on"
  | "blocks"
  | "handoff"
  | "shares_context"
  | "relates_to";

export interface WorkNode {
  id: string;
  project_id: string;
  parent_id: string | null;
  task_id: string | null;
  thread_id: string | null;
  kind: WorkNodeKind;
  title: string;
  description: string | null;
  status: TaskStatus;
  priority: TaskPriority;
  primary_agent: AgentKind | null;
  position_x: number;
  position_y: number;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

export interface NewWorkNode {
  project_id: string;
  parent_id?: string | null;
  kind?: WorkNodeKind | null;
  title: string;
  description?: string | null;
  priority?: TaskPriority;
  primary_agent?: AgentKind | null;
  repo_ids?: string[];
  position_x?: number | null;
  position_y?: number | null;
}

export interface WorkNodeUpdate {
  parent_id?: string | null;
  title?: string | null;
  description?: string | null;
  status?: TaskStatus | null;
  priority?: TaskPriority | null;
  primary_agent?: AgentKind | null;
  position_x?: number | null;
  position_y?: number | null;
  sort_order?: number | null;
}

export interface WorkEdge {
  id: string;
  project_id: string;
  source_id: string;
  target_id: string;
  kind: WorkEdgeKind;
  label: string | null;
  created_at: string;
  updated_at: string;
}

export interface NewWorkEdge {
  project_id: string;
  source_id: string;
  target_id: string;
  kind: WorkEdgeKind;
  label?: string | null;
}

export interface WorkNodeRepoBinding {
  node_id: string;
  repo_id: string;
  repo_name: string;
  worktree_path: string | null;
  branch: string | null;
  base_ref: string | null;
  workspace_backend: ExecutionBackend;
}

export interface WorkRun {
  id: string;
  node_id: string;
  task_id: string | null;
  thread_id: string | null;
  agent_kind: AgentKind;
  run_ref: string;
  state: string;
  started_at: string;
  ended_at: string | null;
}

export interface ContextInclusion {
  source_kind: string;
  entity_id: string | null;
  title: string;
  snippet: string;
  reason: string;
  score: number;
  bytes: number;
}

export interface ContextPacket {
  id: string;
  node_id: string;
  budget_bytes: number;
  used_bytes: number;
  summary: string;
  inclusions: ContextInclusion[];
  created_at: string;
}

export interface WorkGraph {
  project_id: string;
  nodes: WorkNode[];
  edges: WorkEdge[];
  repo_bindings: WorkNodeRepoBinding[];
}

export interface WorkNodeDiff {
  task: TaskDiff | null;
  thread: AgentThreadDiff | null;
}

export type ApprovalKind = "command" | "file_change" | "tool";
export type ApprovalDecision = "allow" | "allow_for_session" | "deny" | "abort";
export type ApprovalResolution = "decided" | "cancelled" | "timed_out";

export interface ApprovalRequest {
  id: string;
  agent: AgentKind;
  project_id?: string | null;
  work_node_id?: string | null;
  task_id?: string | null;
  thread_id?: string | null;
  session_id?: string | null;
  kind: ApprovalKind;
  tool_name: string;
  command?: string[] | null;
  cwd?: string | null;
  input: unknown;
  reason?: string | null;
  created_at: string;
}

export type AppEvent =
  | { type: "agent_thread_created"; data: AgentThread }
  | { type: "agent_thread_updated"; data: AgentThread }
  | { type: "agent_thread_event"; data: AgentThreadEvent }
  | { type: "approval_requested"; data: ApprovalRequest }
  | { type: "approval_resolved"; data: { id: string; resolution: ApprovalResolution; decision?: ApprovalDecision | null } }
  | { type: "work_node_created"; data: WorkNode }
  | { type: "work_node_updated"; data: WorkNode }
  | { type: "repo_connected"; data: Repo }
  | { type: "activity"; data: unknown }
  | { type: string; data: unknown };

export interface ThreadDetails {
  events: AgentThreadEvent[];
  repos: AgentThreadRepo[];
  turns: AgentTurn[];
  queued: QueuedTurn[];
  diff: AgentThreadDiff | null;
  approvals: ApprovalRequest[];
}

export interface WorkbenchSnapshot {
  trusted: boolean;
  defaults: WorkbenchDefaults;
  project: Project | null;
  selectedThreadId: string | null;
  threads: AgentThread[];
  repos: Repo[];
  agents: AgentStatus[];
  runDefaults: AgentRunDefaults[];
  limitPolicy: LimitPolicy | null;
  sandboxPolicy: SandboxPolicy | null;
  sandboxRuntime: SandboxRuntimeStatus | null;
  details: ThreadDetails | null;
  github: GithubAuthStatus | null;
  error: string | null;
}

export interface WorkbenchDefaults {
  agent: AgentKind;
  permission: PermissionPolicy;
  execution_backend: ExecutionBackend;
  model: string | null;
  reasoning: string | null;
}
