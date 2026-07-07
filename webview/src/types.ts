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
export type PermissionPolicy = "read_only" | "workspace_write" | "autonomous";
export type ExecutionBackend = "host" | "docker_sandbox";
export type AvailabilityState = "unknown" | "available" | "limited";
export type LocalModelProvider = "ollama" | "lm_studio";

export interface WorkbenchDefaults {
  agent: AgentKind;
  permission: PermissionPolicy;
  execution_backend: ExecutionBackend;
  model: string | null;
  reasoning: string | null;
  local_provider: LocalModelProvider | null;
  local_base_url: string | null;
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

export interface AgentModelOption {
  id: string;
  label: string;
  aliases: string[];
  family: string | null;
  default: boolean;
  available: boolean;
  source: string;
  reasoning: string[];
  local_provider?: LocalModelProvider | null;
  local_base_url?: string | null;
}

export interface AgentModelCatalog {
  agent: AgentKind;
  default_model: string | null;
  default_reasoning: string | null;
  models: AgentModelOption[];
  reasoning: string[];
  binary_path: string | null;
  version: string | null;
  source: string;
  detected_at: string;
  error: string | null;
}

export interface LocalModelInfo {
  id: string;
  name: string;
  family: string | null;
  parameter_size: string | null;
  quantization: string | null;
  size: number | null;
  loaded: boolean;
}

export interface LocalModelStatus {
  provider: LocalModelProvider;
  label: string;
  base_url: string;
  server_running: boolean;
  cli_installed: boolean;
  cli_path: string | null;
  authenticated: boolean;
  version: string | null;
  models: LocalModelInfo[];
  error: string | null;
}

export interface LimitPolicy {
  auto_switch: boolean;
  switch_back: boolean;
  agent_priority: AgentKind[];
  resume_with_earliest: boolean;
  unknown_reset_retry_secs: number;
  keep_awake: boolean;
}

export interface CloudPolicy {
  enabled: boolean;
  continue_on_sleep: boolean;
  continue_on_shutdown: boolean;
  allow_cross_provider: boolean;
  checkpoint_interval_secs: number;
  monitor_poll_secs: number;
  stall_timeout_secs: number;
  max_concurrent_cloud_runs: number;
  codex_env_id: string | null;
  require_approval: boolean;
}

export interface CloudAvailability {
  agent: AgentKind;
  ready: boolean;
  authenticated: boolean;
  blockers: string[];
  checked_at: string;
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
  local_provider: LocalModelProvider | null;
  local_base_url: string | null;
  original_agent: AgentKind | null;
  fallback_agent: AgentKind | null;
  original_model?: string | null;
  fallback_model?: string | null;
  original_local_provider?: LocalModelProvider | null;
  fallback_local_provider?: LocalModelProvider | null;
  original_local_base_url?: string | null;
  fallback_local_base_url?: string | null;
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
  model?: string | null;
  reasoning?: string | null;
  local_provider?: LocalModelProvider | null;
  local_base_url?: string | null;
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

export interface AgentThreadRepoApplyResult {
  repo_id: string;
  repo_name: string;
  target_path: string | null;
  worktree_path: string | null;
  files: FileChange[];
  applied: boolean;
  blocker: string | null;
}

export interface AgentThreadApplyResult {
  thread_id: string;
  applied: boolean;
  repos: AgentThreadRepoApplyResult[];
  blockers: string[];
}

export type ApprovalKind = "command" | "file_change" | "tool";
export type ApprovalDecision = "allow" | "allow_for_session" | "deny" | "abort";

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

export interface ThreadDetails {
  events: AgentThreadEvent[];
  repos: AgentThreadRepo[];
  turns: AgentTurn[];
  queued: QueuedTurn[];
  diff: AgentThreadDiff | null;
  diffState?: "idle" | "loading" | "ready" | "error";
  applyResult?: AgentThreadApplyResult | null;
  approvals: ApprovalRequest[];
}

export interface WorkbenchSnapshot {
  trusted: boolean;
  defaults: WorkbenchDefaults;
  project: unknown | null;
  selectedThreadId: string | null;
  threads: AgentThread[];
  repos: Repo[];
  agents: AgentStatus[];
  runDefaults: AgentRunDefaults[];
  modelCatalog?: AgentModelCatalog[];
  localModels?: LocalModelStatus[];
  detectionState?: "idle" | "loading" | "ready" | "error";
  defaultRepoIds?: string[];
  limitPolicy: LimitPolicy | null;
  sandboxPolicy: SandboxPolicy | null;
  sandboxRuntime: SandboxRuntimeStatus | null;
  cloudPolicy: CloudPolicy | null;
  cloudAvailability: CloudAvailability[];
  details: ThreadDetails | null;
  github: unknown | null;
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

export type ExtensionMessage =
  | { type: "snapshot"; snapshot: WorkbenchSnapshot }
  | { type: "githubRepos"; repos: GithubRepository[]; status: unknown }
  | { type: "sandboxLoginPrompt"; prompt: { code: string; url: string }; codex: boolean }
  | { type: "notice"; message: string }
  | { type: "error"; message: string };
