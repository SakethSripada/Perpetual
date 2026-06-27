import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import * as vscode from "vscode";
import type { DaemonApi } from "./protocol";
import type { DaemonManager } from "./daemonManager";
import type {
  AgentKind,
  AgentThread,
  AppEvent,
  ExecutionBackend,
  GithubAuthStatus,
  GithubRepository,
  LimitPolicy,
  NewGithubRepo,
  PermissionPolicy,
  SandboxPolicy,
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
};

type WebviewMessage =
  | { type: "ready" | "refresh" | "newSession" | "connectWorkspaceRepos" }
  | { type: "selectThread"; threadId: string | null }
  | SubmitMessage
  | { type: "stopThread"; threadId: string }
  | { type: "deleteThread"; threadId: string; force?: boolean }
  | { type: "assignRepos"; threadId: string; repoIds: string[] }
  | { type: "githubList" }
  | { type: "connectGithubRepo"; repo: GithubRepository }
  | { type: "setLimitPolicy"; policy: LimitPolicy }
  | { type: "setSandboxPolicy"; policy: SandboxPolicy }
  | { type: "sandboxLogin"; codex?: boolean }
  | { type: "openPath"; path: string }
  | { type: "openSettings" | "openPanel" }
  | { type: "deleteQueuedTurn"; id: string }
  | { type: "updateQueuedTurn"; id: string; message: string }
  | { type: "reorderQueuedTurns"; threadId: string; orderedIds: string[] };

export class WorkbenchController implements vscode.Disposable {
  private readonly snapshots = new vscode.EventEmitter<WorkbenchSnapshot>();
  private refreshTimer: NodeJS.Timeout | null = null;
  private lastSyncedSettings = "";
  private disposed = false;

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
        case "ready":
        case "refresh":
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
        case "deleteThread":
          await this.withClient((client) => client.deleteAgentThread(message.threadId, !!message.force));
          await this.selectThread(null);
          this.notice(reply, "Deleted the session.");
          await this.refresh();
          return;
        case "assignRepos":
          await this.withClient((client) => client.assignThreadRepos(message.threadId, message.repoIds));
          await this.refresh();
          return;
        case "connectWorkspaceRepos":
          await this.connectWorkspaceRepos();
          await this.refresh();
          return;
        case "githubList":
          await this.postGithubRepos(reply);
          return;
        case "connectGithubRepo":
          await this.connectGithubRepo(message.repo);
          await this.refresh();
          return;
        case "setLimitPolicy":
          await this.withClient((client) => client.setLimitPolicy(message.policy));
          this.lastSyncedSettings = "";
          await this.refresh();
          return;
        case "setSandboxPolicy":
          await this.withClient((client) => client.setSandboxPolicy(message.policy));
          this.lastSyncedSettings = "";
          await this.refresh();
          return;
        case "sandboxLogin":
          await this.startSandboxLogin(!!message.codex, reply);
          return;
        case "openPath":
          await vscode.env.openExternal(vscode.Uri.file(message.path));
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

  async connectLocalRepoInteractive(): Promise<void> {
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
    await client.connectLocalRepo({ project_id: project.id, path: root });
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
      await this.autoConnectWorkspaceRepos(client, project.id);

      const [threads, repos, agents, runDefaults, limitPolicy, sandboxPolicy, sandboxRuntime] =
        await Promise.all([
          client.listAgentThreads(project.id),
          client.listRepos(project.id),
          client.detectAgents().catch((err) => {
            this.output.appendLine(`[workbench] agent detection failed: ${formatError(err)}`);
            return [];
          }),
          client.agentRunDefaults().catch(() => []),
          client.getLimitPolicy().catch(() => null),
          client.getSandboxPolicy().catch(() => null),
          client.detectSandboxRuntime().catch(() => null),
        ]);

      const selectedThreadId = pickSelectedThread(
        this.context.workspaceState.get<string | null>(SELECTED_THREAD_KEY, null),
        threads
      );
      if (selectedThreadId !== this.context.workspaceState.get(SELECTED_THREAD_KEY)) {
        await this.context.workspaceState.update(SELECTED_THREAD_KEY, selectedThreadId);
      }

      const details = selectedThreadId
        ? await loadThreadDetails(client, selectedThreadId, this.output)
        : null;

      return {
        trusted: true,
        defaults,
        project,
        selectedThreadId,
        threads,
        repos,
        agents,
        runDefaults,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
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
    const defaults = getDefaults();
    const agent = message.agent ?? defaults.agent;
    const permission = message.permission ?? defaults.permission;
    const executionBackend = sanitizeBackend(agent, message.executionBackend ?? defaults.execution_backend);
    const model = blankToNull(message.model ?? defaults.model);
    const reasoning = blankToNull(message.reasoning ?? defaults.reasoning);

    const repoIds = message.repoIds ?? [];
    if (message.threadId) {
      await client.updateAgentThread(message.threadId, {
        preferred_agent: agent,
        permission,
        execution_backend: executionBackend,
        model,
        reasoning,
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

  private async connectGithubRepo(repo: GithubRepository): Promise<void> {
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
    await client.connectGithubRepo(token, input);
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

  private async withClient<T>(fn: (client: DaemonApi) => Promise<T>): Promise<T> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    return fn(client);
  }

  private async syncSettings(client: DaemonApi): Promise<void> {
    const settings = getSettingsSnapshot();
    const encoded = JSON.stringify(settings);
    if (encoded === this.lastSyncedSettings) return;

    const [limitPolicy, sandboxPolicy] = await Promise.all([
      client.getLimitPolicy().catch(() => null),
      client.getSandboxPolicy().catch(() => null),
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

  private async selectThread(threadId: string | null): Promise<void> {
    await this.context.workspaceState.update(SELECTED_THREAD_KEY, threadId);
  }

  private onDaemonEvent(event: AppEvent): void {
    if (event.type === "activity") return;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      void this.refresh();
    }, event.type === "agent_thread_event" ? 350 : 100);
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

async function loadThreadDetails(client: DaemonApi, threadId: string, output: vscode.OutputChannel) {
  const [events, repos, turns, queued, diff] = await Promise.all([
    client.listThreadEvents(threadId).catch((err) => {
      output.appendLine(`[workbench] listThreadEvents failed: ${formatError(err)}`);
      return [];
    }),
    client.listThreadRepos(threadId).catch(() => []),
    client.listThreadTurns(threadId).catch(() => []),
    client.listQueuedTurns(threadId).catch(() => []),
    client.threadDiff(threadId).catch(() => null),
  ]);
  return { events, repos, turns, queued, diff };
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
    limitPolicy: null,
    sandboxPolicy: null,
    sandboxRuntime: null,
    details: null,
    github: null,
    error,
  };
}

function pickSelectedThread(selected: string | null, threads: AgentThread[]): string | null {
  if (selected && threads.some((thread) => thread.id === selected)) return selected;
  return threads[0]?.id ?? null;
}

async function gitRoot(folder: string): Promise<string> {
  try {
    const { stdout } = await execFileAsync("git", ["-C", folder, "rev-parse", "--show-toplevel"]);
    return stdout.trim() || folder;
  } catch {
    return folder;
  }
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

function formatError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
