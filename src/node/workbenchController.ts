import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import * as vscode from "vscode";
import type { DaemonApi } from "./protocol";
import type { DaemonManager } from "./daemonManager";
import type {
  AgentKind,
  AgentModelCatalog,
  AgentRunDefaults,
  AgentStatus,
  AgentThreadApplyResult,
  AgentThreadDiff,
  AgentThread,
  AppEvent,
  ApprovalDecision,
  CloudAvailability,
  CloudPolicy,
  ExecutionBackend,
  GithubAuthStatus,
  GithubRepository,
  LimitPolicy,
  LocalModelStatus,
  LocalModelProvider,
  NewGithubRepo,
  PermissionPolicy,
  SandboxPolicy,
  SandboxRuntimeStatus,
  ThreadDetails,
  WorkbenchDefaults,
  WorkbenchSnapshot,
} from "./types";

const execFileAsync = promisify(execFile);
const SELECTED_THREAD_KEY = "agentmanager.selectedThreadId";

export type WebviewReply = (message: unknown) => void;

type SubmitMessage = {
  type: "submit";
  message: string;
  threadId?: string | null;
  repoIds?: string[];
  agent?: AgentKind;
  permission?: PermissionPolicy;
  executionBackend?: ExecutionBackend;
  model?: string | null;
  reasoning?: string | null;
  localProvider?: LocalModelProvider | null;
  localBaseUrl?: string | null;
};

export type NativeAgentMode = "open" | "plan" | "resume" | "agents";

type WebviewMessage =
  | { type: "ready" | "refresh" | "newSession" | "connectLocalRepo" | "connectWorkspaceRepos" }
  | { type: "selectThread"; threadId: string | null }
  | SubmitMessage
  | { type: "stopThread"; threadId: string }
  | { type: "deleteThread"; threadId: string; force?: boolean }
  | { type: "assignRepos"; threadId: string; repoIds: string[] }
  | { type: "loadDiff"; threadId: string }
  | { type: "applyThreadChanges"; threadId: string }
  | { type: "githubList" }
  | { type: "connectGithubRepo"; repo: GithubRepository }
  | { type: "setLimitPolicy"; policy: LimitPolicy }
  | { type: "setSandboxPolicy"; policy: SandboxPolicy }
  | { type: "setCloudPolicy"; policy: CloudPolicy }
  | { type: "sandboxLogin"; codex?: boolean }
  | { type: "openPath"; path: string }
  | { type: "openNativeAgent"; agent: AgentKind; mode: NativeAgentMode; threadId?: string | null }
  | { type: "openSettings" | "openPanel" }
  | { type: "deleteQueuedTurn"; id: string }
  | { type: "updateQueuedTurn"; id: string; message: string }
  | { type: "reorderQueuedTurns"; threadId: string; orderedIds: string[] }
  | { type: "resolveApproval"; id: string; decision: ApprovalDecision };

// Agent/sandbox detection shells out to CLIs, so we cache it briefly to keep the
// frequent event-driven refreshes from re-probing on every tick.
const DETECTION_TTL_MS = 15_000;

type DetectionCache = {
  at: number;
  agents: AgentStatus[];
  runDefaults: AgentRunDefaults[];
  limitPolicy: LimitPolicy | null;
  sandboxPolicy: SandboxPolicy | null;
  sandboxRuntime: SandboxRuntimeStatus | null;
  cloudPolicy: CloudPolicy | null;
  cloudAvailability: CloudAvailability[];
  modelCatalog: AgentModelCatalog[];
  localModels: LocalModelStatus[];
  state: "loading" | "ready" | "error";
};

type DiffCacheEntry = {
  state: "loading" | "ready" | "error";
  diff: AgentThreadDiff | null;
};

export class WorkbenchController implements vscode.Disposable {
  private readonly snapshots = new vscode.EventEmitter<WorkbenchSnapshot>();
  private refreshTimer: NodeJS.Timeout | null = null;
  private lastSyncedSettings = "";
  private disposed = false;
  private detectionCache: DetectionCache | null = null;
  private diffCache = new Map<string, DiffCacheEntry>();
  private applyResults = new Map<string, AgentThreadApplyResult>();
  private reposConnected = false;

  readonly onSnapshot = this.snapshots.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly daemon: DaemonManager,
    private readonly output: vscode.OutputChannel
  ) {
    context.subscriptions.push(
      daemon.onEvent((event) => this.onDaemonEvent(event)),
      vscode.workspace.onDidGrantWorkspaceTrust(() => void this.refresh()),
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration("agentmanager")) {
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          void this.refresh();
        }
      })
    );
  }

  dispose(): void {
    this.disposed = true;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.snapshots.dispose();
  }

  async handleMessage(message: WebviewMessage, reply?: WebviewReply): Promise<void> {
    try {
      switch (message.type) {
        case "refresh":
          // Manual refresh should re-probe agents/sandbox, not serve the cache.
          this.detectionCache = null;
          this.reposConnected = false;
          await this.refresh();
          return;
        case "ready":
          await this.refresh();
          return;
        case "newSession":
          await this.selectThread(null);
          await this.refresh();
          return;
        case "selectThread":
          await this.selectThread(message.threadId);
          await this.refresh();
          return;
        case "submit":
          await this.submit(message);
          await this.refresh();
          return;
        case "stopThread":
          await this.withClient((client) => client.stopAgentThread(message.threadId));
          this.notice(reply, "Stopped the active run.");
          await this.refresh();
          return;
        case "deleteThread": {
          await this.withClient((client) => client.deleteAgentThread(message.threadId, !!message.force));
          // Only drop the selection when the open thread is the one being deleted,
          // so deleting an older session from the menu doesn't yank you out of the
          // session you're currently viewing.
          const current = this.context.workspaceState.get<string | null>(SELECTED_THREAD_KEY, null);
          if (current === message.threadId) await this.selectThread(null);
          this.notice(reply, "Deleted the session.");
          await this.refresh();
          return;
        }
        case "assignRepos":
          await this.withClient((client) => client.assignThreadRepos(message.threadId, message.repoIds));
          this.diffCache.delete(message.threadId);
          await this.refresh();
          return;
        case "loadDiff":
          await this.loadDiff(message.threadId);
          return;
        case "applyThreadChanges": {
          const result = await this.withClient((client) => client.applyThreadChanges(message.threadId));
          this.applyResults.set(message.threadId, result);
          this.notice(
            reply,
            result.applied
              ? "Applied managed changes to the visible repository."
              : result.blockers[0] ?? "No managed changes to apply."
          );
          await this.refresh();
          return;
        }
        case "connectWorkspaceRepos":
          await this.connectWorkspaceRepos();
          await this.refresh();
          return;
        case "connectLocalRepo":
          await this.connectLocalRepoInteractive(reply);
          return;
        case "githubList":
          await this.postGithubRepos(reply);
          return;
        case "connectGithubRepo":
          await this.connectGithubRepo(message.repo, reply);
          await this.refresh();
          return;
        case "setLimitPolicy":
          await this.withClient((client) => client.setLimitPolicy(message.policy));
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "setSandboxPolicy":
          await this.withClient((client) => client.setSandboxPolicy(message.policy));
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "setCloudPolicy":
          await this.withClient((client) => client.setCloudPolicy(message.policy));
          // Mirror into VS Code settings so the next settings sync doesn't undo
          // what the user just applied from the in-webview sheet.
          await this.mirrorCloudPolicyToConfig(message.policy);
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "sandboxLogin":
          await this.startSandboxLogin(!!message.codex, reply);
          return;
        case "openPath":
          await vscode.env.openExternal(vscode.Uri.file(message.path));
          return;
        case "openNativeAgent":
          await this.openNativeAgent(message.agent, message.mode, message.threadId ?? null, reply);
          return;
        case "openSettings":
          await vscode.commands.executeCommand("workbench.action.openSettings", "@ext:agentmanager.agentmanager-vscode");
          return;
        case "openPanel":
          await vscode.commands.executeCommand("agentmanager.openWorkbench");
          return;
        case "deleteQueuedTurn":
          await this.withClient((client) => client.deleteQueuedTurn(message.id));
          await this.refresh();
          return;
        case "updateQueuedTurn":
          await this.withClient((client) => client.updateQueuedTurn(message.id, message.message));
          await this.refresh();
          return;
        case "reorderQueuedTurns":
          await this.withClient((client) => client.reorderQueuedTurns(message.threadId, message.orderedIds));
          await this.refresh();
          return;
        case "resolveApproval":
          await this.withClient((client) => client.resolveApproval(message.id, message.decision));
          await this.refresh();
          return;
      }
    } catch (err) {
      const text = err instanceof Error ? err.message : String(err);
      this.output.appendLine(`[workbench] ${text}`);
      reply?.({ type: "error", message: text });
      await this.refresh(text);
    }
  }

  async refresh(error: string | null = null): Promise<void> {
    const snapshot = await this.snapshot(error);
    if (!this.disposed) {
      this.snapshots.fire(snapshot);
    }
  }

  async connectLocalRepoInteractive(reply?: WebviewReply): Promise<void> {
    this.assertTrusted();
    const picked = await vscode.window.showOpenDialog({
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
      openLabel: "Connect Repository",
      title: "Connect Local Repository",
    });
    const folder = picked?.[0]?.fsPath;
    if (!folder) return;

    const root = await gitRoot(folder);
    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    const repo = await client.connectLocalRepo({ project_id: project.id, path: root });
    reply?.({ type: "repoConnected", repo });
    await this.refresh();
  }

  async connectGithubRepoInteractive(): Promise<void> {
    this.assertTrusted();
    const { repos } = await this.githubRepos();
    const items: Array<vscode.QuickPickItem & { repo: GithubRepository }> = repos.map((repo: GithubRepository) => ({
        label: repo.full_name,
        description: repo.private ? "Private" : "Public",
        detail: repo.html_url,
        repo,
      }));
    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: "Select a GitHub repository to connect",
    });
    if (!picked) return;
    await this.connectGithubRepo(picked.repo);
    await this.refresh();
  }

  async newSession(): Promise<void> {
    await this.selectThread(null);
    await this.refresh();
  }

  private async snapshot(error: string | null): Promise<WorkbenchSnapshot> {
    const defaults = getDefaults();
    if (!vscode.workspace.isTrusted) {
      return emptySnapshot(false, defaults, "Trust this workspace to connect repositories and run agent CLIs.");
    }

    try {
      const client = await this.daemon.getClient();
      await this.syncSettings(client);
      const project = await client.ensureWorkbenchProject();
      // Connecting workspace repos shells out to git and rarely changes mid-session;
      // do it once rather than on every event-driven refresh.
      if (!this.reposConnected) {
        await this.autoConnectWorkspaceRepos(client, project.id);
        this.reposConnected = true;
      }

      // Fast path: threads/repos are cheap DB reads; agent + sandbox detection is
      // expensive (CLI probes) so it comes from a short-lived cache.
      const [threads, repos, detection] = await Promise.all([
        client.listAgentThreads(project.id),
        client.listRepos(project.id),
        this.detect(client),
      ]);
      const {
        agents,
        runDefaults,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
        cloudPolicy,
        cloudAvailability,
        modelCatalog,
        localModels,
        state: detectionState,
      } =
        detection;

      const selectedThreadId = pickSelectedThread(
        this.context.workspaceState.get<string | null>(SELECTED_THREAD_KEY, null),
        threads
      );
      if (selectedThreadId !== this.context.workspaceState.get(SELECTED_THREAD_KEY)) {
        await this.context.workspaceState.update(SELECTED_THREAD_KEY, selectedThreadId);
      }

      const details = selectedThreadId
        ? await loadThreadDetails(
            client,
            selectedThreadId,
            this.output,
            this.diffCache.get(selectedThreadId) ?? null,
            this.applyResults.get(selectedThreadId) ?? null
          )
        : null;

      const defaultRepoIds = pickDefaultRepoIds(repos);
      return {
        trusted: true,
        defaults,
        project,
        selectedThreadId,
        threads,
        repos,
        agents,
        runDefaults,
        modelCatalog,
        localModels,
        detectionState,
        defaultRepoIds,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
        cloudPolicy,
        cloudAvailability,
        details,
        github: null,
        error,
      };
    } catch (err) {
      return emptySnapshot(true, defaults, formatError(err));
    }
  }

  private async submit(message: SubmitMessage): Promise<void> {
    this.assertTrusted();
    const text = message.message.trim();
    if (!text) return;

    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    if (!this.reposConnected) {
      await this.autoConnectWorkspaceRepos(client, project.id);
      this.reposConnected = true;
    }
    const defaults = getDefaults();
    const agent = message.agent ?? defaults.agent;
    const permission = message.permission ?? defaults.permission;
    const executionBackend = sanitizeBackend(agent, message.executionBackend ?? defaults.execution_backend);
    const localProvider = agent === "codex"
      ? sanitizeLocalProvider(message.localProvider ?? defaults.local_provider)
      : null;
    const localBaseUrl = localProvider
      ? blankToNull(message.localBaseUrl ?? defaults.local_base_url) ?? defaultLocalBaseUrl(localProvider)
      : null;
    const model = blankToNull(message.model ?? defaults.model);
    const reasoning = blankToNull(message.reasoning ?? defaults.reasoning);
    if (localProvider && !model) {
      throw new Error("Choose a local model before running with Ollama or LM Studio.");
    }

    const repos = await client.listRepos(project.id).catch(() => []);
    const repoIds = resolveSubmittedRepoIds(message.repoIds, repos);
    if (message.threadId) {
      const currentRepos = await client.listThreadRepos(message.threadId).catch(() => []);
      const hasWorktree = currentRepos.some((repo: { worktree_path: string | null }) => !!repo.worktree_path);
      if (!hasWorktree) {
        await client.assignThreadRepos(message.threadId, repoIds);
      }
      await client.updateAgentThread(message.threadId, {
        preferred_agent: agent,
        permission,
        execution_backend: executionBackend,
        model,
        reasoning,
        local_provider: localProvider,
        local_base_url: localBaseUrl,
      });
      await client.sendThreadMessage(message.threadId, agent, permission, text);
      await this.selectThread(message.threadId);
      return;
    }

    const thread = await client.createAgentThread({
      project_id: project.id,
      title: titleFromMessage(text),
      objective: text,
      repo_ids: repoIds,
      preferred_agent: agent,
      permission,
      execution_backend: executionBackend,
      model,
      reasoning,
      local_provider: localProvider,
      local_base_url: localBaseUrl,
    });
    await this.selectThread(thread.id);
    await client.runAgentThread(thread.id, agent, permission, text, executionBackend);
  }

  private async connectWorkspaceRepos(): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    await this.autoConnectWorkspaceRepos(client, project.id, true);
  }

  private async autoConnectWorkspaceRepos(
    client: DaemonApi,
    projectId: string,
    notifyErrors = false
  ): Promise<void> {
    const folders = vscode.workspace.workspaceFolders ?? [];
    if (!folders.length) return;

    const existing = await client.listRepos(projectId).catch(() => []);
    const existingPaths = new Set(
      existing
        .map((repo) => repo.local_path)
        .filter((repoPath): repoPath is string => !!repoPath)
        .map((repoPath) => path.normalize(repoPath))
    );

    for (const folder of folders) {
      try {
        const root = await gitRoot(folder.uri.fsPath);
        const normalized = path.normalize(root);
        if (existingPaths.has(normalized)) continue;
        await client.connectLocalRepo({ project_id: projectId, path: root });
        existingPaths.add(normalized);
      } catch (err) {
        if (notifyErrors) {
          vscode.window.showWarningMessage(`Could not connect ${folder.name}: ${formatError(err)}`);
        }
      }
    }
  }

  private async postGithubRepos(reply?: WebviewReply): Promise<void> {
    const { status, repos } = await this.githubRepos();
    reply?.({ type: "githubRepos", status, repos });
  }

  private async githubRepos(): Promise<{ status: GithubAuthStatus; repos: GithubRepository[] }> {
    this.assertTrusted();
    const token = await this.githubToken();
    const client = await this.daemon.getClient();
    const [status, repos] = await Promise.all([
      client.githubAuthStatus(token),
      client.githubListRepositories(token),
    ]);
    return { status, repos };
  }

  private async connectGithubRepo(repo: GithubRepository, reply?: WebviewReply): Promise<void> {
    this.assertTrusted();
    const token = await this.githubToken();
    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    const input: NewGithubRepo = {
      project_id: project.id,
      name: repo.name,
      full_name: repo.full_name,
      clone_url: repo.clone_url,
      ssh_url: repo.ssh_url,
      default_branch: repo.default_branch,
    };
    const connected = await client.connectGithubRepo(token, input);
    reply?.({ type: "repoConnected", repo: connected });
  }

  private async githubToken(): Promise<string> {
    const session = await vscode.authentication.getSession("github", ["repo"], {
      createIfNone: true,
    });
    return session.accessToken;
  }

  private async startSandboxLogin(codex: boolean, reply?: WebviewReply): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    const prompt = codex ? await client.codexSandboxLogin() : await client.sandboxLogin();
    reply?.({ type: "sandboxLoginPrompt", prompt, codex });
    await vscode.env.openExternal(vscode.Uri.parse(prompt.url));
  }

  async openNativeAgent(
    agent: AgentKind,
    mode: NativeAgentMode = "open",
    threadId: string | null = null,
    reply?: WebviewReply
  ): Promise<void> {
    this.assertTrusted();
    const cwd = await this.nativeWorkingDirectory(threadId);
    const command = this.nativeAgentCommand(agent, mode);
    const terminal = vscode.window.createTerminal({
      name: nativeTerminalName(agent, mode),
      cwd,
      isTransient: false,
    });
    terminal.show(false);
    terminal.sendText(command, true);
    this.notice(reply, `Opened ${nativeTerminalName(agent, mode)} in Terminal.`);
  }

  private nativeAgentCommand(agent: AgentKind, mode: NativeAgentMode): string {
    const binary = shellQuote(this.nativeAgentBinary(agent));
    if (agent === "claude_code") {
      if (mode === "plan") return `${binary} --permission-mode plan`;
      if (mode === "resume") return `${binary} --continue`;
      if (mode === "agents") return `${binary} agents`;
      return binary;
    }
    if (agent === "codex") {
      if (mode === "resume") return `${binary} resume --last`;
      return binary;
    }
    return binary;
  }

  private nativeAgentBinary(agent: AgentKind): string {
    const status = this.detectionCache?.agents.find((item) => item.kind === agent);
    if (status?.binary_path) return status.binary_path;
    if (agent === "claude_code") return "claude";
    if (agent === "codex") return "codex";
    return nativeAgentLabel(agent).toLowerCase();
  }

  private async nativeWorkingDirectory(threadId: string | null): Promise<string | undefined> {
    if (threadId) {
      try {
        const client = await this.daemon.getClient();
        const repos = await client.listThreadRepos(threadId);
        const repoPath = repos.find((repo) => repo.worktree_path)?.worktree_path;
        if (repoPath) return repoPath;
      } catch (err) {
        this.output.appendLine(`[workbench] native cwd lookup failed: ${formatError(err)}`);
      }
    }
    const activePath = vscode.window.activeTextEditor?.document.uri.fsPath;
    if (activePath) return gitRoot(path.dirname(activePath));
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  }

  private async loadDiff(threadId: string): Promise<void> {
    this.diffCache.set(threadId, { state: "loading", diff: null });
    await this.refresh();
    try {
      const diff = await this.withClient((client) => client.threadDiff(threadId));
      this.diffCache.set(threadId, { state: "ready", diff });
    } catch (err) {
      this.output.appendLine(`[workbench] threadDiff failed: ${formatError(err)}`);
      this.diffCache.set(threadId, { state: "error", diff: null });
    }
    await this.refresh();
  }

  private async withClient<T>(fn: (client: DaemonApi) => Promise<T>): Promise<T> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    return fn(client);
  }

  private detectInflight: Promise<DetectionCache> | null = null;

  /**
   * Agent/sandbox detection shells out to CLIs and can take seconds, so it must
   * never block a snapshot. Serve the last-known values immediately; when stale,
   * kick off a background re-probe that fires its own refresh when it lands.
   */
  private detect(client: DaemonApi): DetectionCache {
    const cached = this.detectionCache;
    if (cached) {
      if (Date.now() - cached.at >= DETECTION_TTL_MS && !this.detectInflight) {
        void this.runDetection(client).then(() => void this.refresh());
      }
      return cached;
    }
    const loading = emptyDetectionCache("loading");
    this.detectionCache = loading;
    void this.runDetection(client).then(() => void this.refresh());
    return loading;
  }

  private runDetection(client: DaemonApi): Promise<DetectionCache> {
    if (this.detectInflight) return this.detectInflight;
    this.detectInflight = (async () => {
      const [
        agents,
        runDefaults,
        modelCatalog,
        localModels,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
        cloudPolicy,
        cloudAvailability,
      ] =
        await Promise.all([
          client.detectAgents().catch((err) => {
            this.output.appendLine(`[workbench] agent detection failed: ${formatError(err)}`);
            return [];
          }),
          client.agentRunDefaults().catch(() => []),
          client.agentModelCatalog().catch(() => []),
          client.detectLocalModels().catch(() => []),
          client.getLimitPolicy().catch(() => null),
          client.getSandboxPolicy().catch(() => null),
          client.detectSandboxRuntime().catch(() => null),
          client.getCloudPolicy().catch(() => null),
          client.cloudAvailability().catch(() => []),
        ]);
      const next: DetectionCache = {
        at: Date.now(),
        agents,
        runDefaults,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
        cloudPolicy,
        cloudAvailability,
        modelCatalog,
        localModels,
        state: "ready",
      };
      this.detectionCache = next;
      return next;
    })().catch((err) => {
      this.output.appendLine(`[workbench] detection failed: ${formatError(err)}`);
      const failed = emptyDetectionCache("error");
      this.detectionCache = failed;
      return failed;
    }).finally(() => {
      this.detectInflight = null;
    });
    return this.detectInflight;
  }

  private async syncSettings(client: DaemonApi): Promise<void> {
    const settings = getSettingsSnapshot();
    const encoded = JSON.stringify(settings);
    if (encoded === this.lastSyncedSettings) return;

    const [limitPolicy, sandboxPolicy, cloudPolicy] = await Promise.all([
      client.getLimitPolicy().catch(() => null),
      client.getSandboxPolicy().catch(() => null),
      client.getCloudPolicy().catch(() => null),
    ]);
    if (limitPolicy) {
      await client.setLimitPolicy({
        ...limitPolicy,
        auto_switch: settings.autoSwitchOnLimit,
        switch_back: settings.switchBackOnRecovery,
        resume_with_earliest: settings.resumeWithEarliestAgent,
        unknown_reset_retry_secs: settings.unknownLimitRetrySeconds,
        agent_priority: settings.fallbackPriority,
      });
    }
    if (cloudPolicy) {
      await client.setCloudPolicy({
        ...cloudPolicy,
        enabled: settings.cloudAutoCarryover,
        continue_on_sleep: settings.cloudCarryOverOnSleep,
        continue_on_shutdown: settings.cloudCarryOverOnShutdown,
        allow_cross_provider: settings.cloudProviderStrategy === "switch_provider",
        require_approval: settings.cloudRequireApproval,
        max_concurrent_cloud_runs: settings.cloudMaxConcurrentRuns,
        codex_env_id: blankToNull(settings.cloudCodexEnvId),
      });
    }
    if (sandboxPolicy) {
      await client.setSandboxPolicy({
        ...sandboxPolicy,
        default_backend: settings.defaultExecutionBackend,
        max_concurrent_sandboxes: settings.sandboxMaxConcurrent,
        cpus: settings.sandboxCpus,
        memory: settings.sandboxMemory,
        network_preset: settings.sandboxNetworkPreset,
      });
    }
    this.lastSyncedSettings = encoded;
  }

  /**
   * The VS Code settings are the durable source of truth for the cloud
   * carryover policy (syncSettings pushes them to the daemon on every change),
   * so a policy applied from the webview sheet must be written back to the
   * configuration or the next sync would silently revert it.
   */
  private async mirrorCloudPolicyToConfig(policy: CloudPolicy): Promise<void> {
    const config = vscode.workspace.getConfiguration("agentmanager");
    const target = vscode.ConfigurationTarget.Global;
    await Promise.all([
      config.update("cloud.autoCarryover", policy.enabled, target),
      config.update("cloud.carryOverOnSleep", policy.continue_on_sleep, target),
      config.update("cloud.carryOverOnShutdown", policy.continue_on_shutdown, target),
      config.update(
        "cloud.providerStrategy",
        policy.allow_cross_provider ? "switch_provider" : "same_provider",
        target
      ),
      config.update("cloud.requireApproval", policy.require_approval, target),
      config.update("cloud.maxConcurrentRuns", policy.max_concurrent_cloud_runs, target),
      config.update("cloud.codexEnvId", policy.codex_env_id ?? "", target),
    ]);
  }

  private async selectThread(threadId: string | null): Promise<void> {
    await this.context.workspaceState.update(SELECTED_THREAD_KEY, threadId);
  }

  private onDaemonEvent(event: AppEvent): void {
    if (event.type === "activity") return;
    if (event.type === "agent_thread_event") {
      const data = event.data as { thread_id?: string };
      if (data.thread_id) this.diffCache.delete(data.thread_id);
    }
    if (event.type === "agent_thread_updated") {
      const data = event.data as { id?: string; status?: string };
      if (data.id && data.status === "running") {
        this.diffCache.delete(data.id);
        this.applyResults.delete(data.id);
      }
    }
    // Approvals are interactive — surface them immediately. Streaming thread
    // events are coalesced just enough to avoid thrashing the webview.
    const delay =
      event.type === "approval_requested" || event.type === "approval_resolved"
        ? 0
        : event.type === "agent_thread_event"
          ? 150
          : 80;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      void this.refresh();
    }, delay);
  }

  private notice(reply: WebviewReply | undefined, message: string): void {
    reply?.({ type: "notice", message });
  }

  private assertTrusted(): void {
    if (!vscode.workspace.isTrusted) {
      throw new Error("Trust this workspace before connecting repositories or running agents.");
    }
  }
}

async function loadThreadDetails(
  client: DaemonApi,
  threadId: string,
  output: vscode.OutputChannel,
  diffEntry: DiffCacheEntry | null,
  applyResult: AgentThreadApplyResult | null
): Promise<ThreadDetails> {
  const [events, repos, turns, queued, approvals] = await Promise.all([
    client.listThreadEvents(threadId).catch((err) => {
      output.appendLine(`[workbench] listThreadEvents failed: ${formatError(err)}`);
      return [];
    }),
    client.listThreadRepos(threadId).catch(() => []),
    client.listThreadTurns(threadId).catch(() => []),
    client.listQueuedTurns(threadId).catch(() => []),
    client.listPendingApprovals().catch(() => []),
  ]);
  return {
    events,
    repos,
    turns,
    queued,
    diff: diffEntry?.diff ?? null,
    diffState: diffEntry?.state ?? "idle",
    applyResult,
    approvals: approvals.filter((approval) => approval.thread_id === threadId),
  };
}

function emptyDetectionCache(state: DetectionCache["state"]): DetectionCache {
  return {
    at: 0,
    agents: [],
    runDefaults: [],
    limitPolicy: null,
    sandboxPolicy: null,
    sandboxRuntime: null,
    cloudPolicy: null,
    cloudAvailability: [],
    modelCatalog: [],
    localModels: [],
    state,
  };
}

function emptySnapshot(trusted: boolean, defaults: WorkbenchDefaults, error: string): WorkbenchSnapshot {
  return {
    trusted,
    defaults,
    project: null,
    selectedThreadId: null,
    threads: [],
    repos: [],
    agents: [],
    runDefaults: [],
    modelCatalog: [],
    localModels: [],
    detectionState: "idle",
    defaultRepoIds: [],
    limitPolicy: null,
    sandboxPolicy: null,
    sandboxRuntime: null,
    cloudPolicy: null,
    cloudAvailability: [],
    details: null,
    github: null,
    error,
  };
}

function pickSelectedThread(selected: string | null, threads: AgentThread[]): string | null {
  if (selected && threads.some((thread) => thread.id === selected)) return selected;
  return threads[0]?.id ?? null;
}

function pickDefaultRepoIds(repos: Array<{ id: string; local_path: string | null }>): string[] {
  if (repos.length === 1) return [repos[0].id];
  const activePath = vscode.window.activeTextEditor?.document.uri.fsPath;
  if (!activePath) return [];
  const normalizedActive = path.normalize(activePath);
  const matches = repos.filter((repo) => {
    if (!repo.local_path) return false;
    const root = path.normalize(repo.local_path);
    return normalizedActive === root || normalizedActive.startsWith(`${root}${path.sep}`);
  });
  return matches.length === 1 ? [matches[0].id] : [];
}

function resolveSubmittedRepoIds(
  submitted: string[] | undefined,
  repos: Array<{ id: string; local_path: string | null }>
): string[] {
  const known = new Set(repos.map((repo) => repo.id));
  const picked = submitted === undefined ? pickDefaultRepoIds(repos) : submitted;
  const repoIds = Array.from(new Set(picked)).filter((id) => known.has(id));
  if (repos.length > 0 && repoIds.length === 0) {
    throw new Error("Select at least one connected repository before starting the agent.");
  }
  return repoIds;
}

async function gitRoot(folder: string): Promise<string> {
  try {
    const { stdout } = await execFileAsync("git", ["-C", folder, "rev-parse", "--show-toplevel"]);
    return stdout.trim() || folder;
  } catch {
    return folder;
  }
}

function nativeTerminalName(agent: AgentKind, mode: NativeAgentMode): string {
  const label = nativeAgentLabel(agent);
  if (mode === "plan") return `${label} Plan`;
  if (mode === "resume") return `${label} Resume`;
  if (mode === "agents") return `${label} Agents`;
  return label;
}

function nativeAgentLabel(agent: AgentKind): string {
  if (agent === "claude_code") return "Claude Code";
  if (agent === "codex") return "Codex";
  if (agent === "open_code") return "OpenCode";
  return agent.charAt(0).toUpperCase() + agent.slice(1);
}

function shellQuote(value: string): string {
  if (/^[A-Za-z0-9_./:-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

function titleFromMessage(message: string): string {
  const singleLine = message.replace(/\s+/g, " ").trim();
  return singleLine.length > 56 ? `${singleLine.slice(0, 53)}...` : singleLine || "New session";
}

function getDefaults(): WorkbenchDefaults {
  const config = vscode.workspace.getConfiguration("agentmanager");
  return {
    agent: config.get<AgentKind>("defaultAgent", "claude_code"),
    permission: config.get<PermissionPolicy>("defaultPermission", "workspace_write"),
    execution_backend: config.get<ExecutionBackend>("defaultExecutionBackend", "host"),
    model: blankToNull(config.get<string>("defaultModel", "")),
    reasoning: blankToNull(config.get<string>("defaultReasoning", "medium")),
    local_provider: sanitizeLocalProvider(config.get<string>("defaultLocalProvider", "")),
    local_base_url: blankToNull(config.get<string>("defaultLocalBaseUrl", "")),
  };
}

function getSettingsSnapshot() {
  const config = vscode.workspace.getConfiguration("agentmanager");
  return {
    defaultExecutionBackend: config.get<ExecutionBackend>("defaultExecutionBackend", "host"),
    autoSwitchOnLimit: config.get<boolean>("autoSwitchOnLimit", true),
    switchBackOnRecovery: config.get<boolean>("switchBackOnRecovery", true),
    resumeWithEarliestAgent: config.get<boolean>("resumeWithEarliestAgent", true),
    unknownLimitRetrySeconds: config.get<number>("unknownLimitRetrySeconds", 600),
    fallbackPriority: config.get<AgentKind[]>("fallbackPriority", ["claude_code", "codex"]),
    cloudAutoCarryover: config.get<boolean>("cloud.autoCarryover", false),
    cloudCarryOverOnSleep: config.get<boolean>("cloud.carryOverOnSleep", true),
    cloudCarryOverOnShutdown: config.get<boolean>("cloud.carryOverOnShutdown", true),
    cloudProviderStrategy: config.get<string>("cloud.providerStrategy", "same_provider"),
    cloudRequireApproval: config.get<boolean>("cloud.requireApproval", false),
    cloudMaxConcurrentRuns: config.get<number>("cloud.maxConcurrentRuns", 2),
    cloudCodexEnvId: config.get<string>("cloud.codexEnvId", ""),
    sandboxMaxConcurrent: config.get<number>("sandbox.maxConcurrent", 2),
    sandboxCpus: config.get<number>("sandbox.cpus", 2),
    sandboxMemory: config.get<string>("sandbox.memory", "4g"),
    sandboxNetworkPreset: config.get<string>("sandbox.networkPreset", "balanced"),
  };
}

function sanitizeBackend(agent: AgentKind, backend: ExecutionBackend): ExecutionBackend {
  if (backend === "docker_sandbox" && agent !== "codex") {
    return "host";
  }
  return backend;
}

function blankToNull(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

function sanitizeLocalProvider(value: string | null | undefined): LocalModelProvider | null {
  return value === "ollama" || value === "lm_studio" ? value : null;
}

function defaultLocalBaseUrl(provider: LocalModelProvider): string {
  return provider === "lm_studio" ? "http://127.0.0.1:1234" : "http://127.0.0.1:11434";
}

function formatError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
