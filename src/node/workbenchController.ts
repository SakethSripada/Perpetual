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
  AgentThreadEvent,
  AgentThreadApplyResult,
  AgentThreadDiff,
  AgentThread,
  AppEvent,
  ApprovalDecision,
  CloudAvailability,
  CloudPolicy,
  CollaborationAssignment,
  ExecutionBackend,
  GithubAuthStatus,
  GithubRepository,
  LimitPolicy,
  LocalModelPolicy,
  LocalModelStatus,
  LocalModelProvider,
  NewGithubRepo,
  PermissionPolicy,
  ProviderUsage,
  SandboxPolicy,
  SandboxRuntimeStatus,
  TaskBudget,
  ThreadDetails,
  WorkbenchDefaults,
  WorkbenchSnapshot,
} from "./types";

const execFileAsync = promisify(execFile);
const SELECTED_THREAD_KEY = "perpetual.selectedThreadId";

export type WebviewReply = (message: unknown) => void;

type SubmitMessage = {
  type: "submit";
  message: string;
  clientMessageId?: string | null;
  threadId?: string | null;
  repoIds?: string[];
  agent?: AgentKind;
  permission?: PermissionPolicy;
  executionBackend?: ExecutionBackend;
  model?: string | null;
  reasoning?: string | null;
  localProvider?: LocalModelProvider | null;
  localBaseUrl?: string | null;
  taskBudget?: TaskBudget;
  deviceId?: string | null;
};

type WebviewMessage =
  | {
      type:
        | "ready"
        | "refresh"
        | "newSession"
        | "connectLocalRepo"
        | "connectWorkspaceRepos";
    }
  | { type: "selectThread"; threadId: string | null }
  | SubmitMessage
  | { type: "stopThread"; threadId: string }
  | { type: "deleteThread"; threadId: string; force?: boolean }
  | { type: "assignRepos"; threadId: string; repoIds: string[] }
  | { type: "loadDiff"; threadId: string }
  | { type: "applyThreadChanges"; threadId: string }
  | { type: "githubList" }
  | { type: "connectGithubRepo"; repo: GithubRepository }
  | { type: "deleteRepo"; repoId: string }
  | { type: "clearRepos" }
  | { type: "setLimitPolicy"; policy: LimitPolicy }
  | { type: "setSandboxPolicy"; policy: SandboxPolicy }
  | { type: "setCloudPolicy"; policy: CloudPolicy }
  | { type: "setLocalModelPolicy"; policy: LocalModelPolicy }
  | { type: "sandboxLogin"; codex?: boolean }
  | { type: "signInAgent"; agent: AgentKind }
  | { type: "githubSignIn" | "refreshReadiness" }
  | { type: "launchCloudHandoff"; threadId: string; agent?: AgentKind | null }
  | { type: "reclaimCloudRun"; threadId: string }
  | { type: "openPath"; path: string }
  | { type: "openExternal"; url: string }
  | { type: "openSettings" | "openPanel" }
  | { type: "deleteQueuedTurn"; id: string }
  | { type: "updateQueuedTurn"; id: string; message: string }
  | { type: "reorderQueuedTurns"; threadId: string; orderedIds: string[] }
  | { type: "resolveApproval"; id: string; decision: ApprovalDecision }
  | { type: "hostCollaboration" | "copyCollaborationInvite" }
  | { type: "joinCollaboration"; invite: string }
  | { type: "leaveCollaboration" }
  | { type: "revokeCollaborationDevice"; deviceId: string }
  | { type: "cancelCollaborationAssignment"; assignmentId: string }
  | { type: "dismissCollaborationAssignmentIssue"; assignmentId: string }
  | { type: "retryCollaborationAssignment"; assignmentId: string }
  | { type: "addCollaborationRepoAndRetry"; assignmentId: string }
  | { type: "applyCollaborationChangeSet"; changeSetId: string; overwrite?: boolean }
  | { type: "rejectCollaborationChangeSet"; changeSetId: string };

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
  localModelPolicy: LocalModelPolicy | null;
  state: "loading" | "ready" | "error";
};

type DiffCacheEntry = {
  state: "loading" | "ready" | "error";
  diff: AgentThreadDiff | null;
};

export class WorkbenchController implements vscode.Disposable {
  private readonly snapshots = new vscode.EventEmitter<WorkbenchSnapshot>();
  private readonly threadEvents = new vscode.EventEmitter<AgentThreadEvent>();
  private refreshTimer: NodeJS.Timeout | null = null;
  private refreshSequence = 0;
  private messageQueue: Promise<void> = Promise.resolve();
  private lastSyncedSettings = "";
  private disposed = false;
  private detectionCache: DetectionCache | null = null;
  private diffCache = new Map<string, DiffCacheEntry>();
  private applyResults = new Map<string, AgentThreadApplyResult>();
  private autoApplyInFlight = new Set<string>();
  private autoAppliedThreads = new Set<string>();
  private repoAssignmentsPending = new Map<string, string[]>();
  private repoAssignmentsInFlight = new Map<string, Promise<void>>();
  private workspaceReposReady: Promise<void> | null = null;
  private autoConnectSuspended = false;

  readonly onSnapshot = this.snapshots.event;
  readonly onThreadEvent = this.threadEvents.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly daemon: DaemonManager,
    private readonly output: vscode.OutputChannel,
  ) {
    context.subscriptions.push(
      daemon.onEvent((event) => this.onDaemonEvent(event)),
      vscode.workspace.onDidGrantWorkspaceTrust(() => void this.refresh()),
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.workspaceReposReady = null;
        this.autoConnectSuspended = false;
        void this.refresh();
      }),
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration("perpetual")) {
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          void this.refresh();
        }
      }),
    );
  }

  dispose(): void {
    this.disposed = true;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.snapshots.dispose();
    this.threadEvents.dispose();
  }

  async handleMessage(
    message: WebviewMessage,
    reply?: WebviewReply,
  ): Promise<void> {
    const next = this.messageQueue.then(() =>
      this.handleMessageNow(message, reply),
    );
    this.messageQueue = next.catch(() => undefined);
    return next;
  }

  private async handleMessageNow(
    message: WebviewMessage,
    reply?: WebviewReply,
  ): Promise<void> {
    try {
      switch (message.type) {
        case "refresh":
          // Manual refresh should re-probe agents/sandbox, not serve the cache.
          this.detectionCache = null;
          this.workspaceReposReady = null;
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
          await this.withClient(async (client) => {
            const remote = (await client.listCollaborationAssignments(null, true))
              .find(
                (assignment) =>
                  assignment.thread_id === message.threadId &&
                  (assignment.status === "queued" || assignment.status === "running"),
              );
            if (remote) await client.cancelCollaborationAssignment(remote.id);
            else await client.stopAgentThread(message.threadId);
          });
          this.notice(reply, "Stopped the active run.");
          await this.refresh();
          return;
        case "deleteThread": {
          await this.withClient((client) =>
            client.deleteAgentThread(message.threadId, !!message.force),
          );
          // Only drop the selection when the open thread is the one being deleted,
          // so deleting an older session from the menu doesn't yank you out of the
          // session you're currently viewing.
          const current = this.context.workspaceState.get<string | null>(
            SELECTED_THREAD_KEY,
            null,
          );
          if (current === message.threadId) await this.selectThread(null);
          this.notice(reply, "Deleted the session.");
          await this.refresh();
          return;
        }
        case "assignRepos":
          await this.assignRepos(message.threadId, message.repoIds);
          return;
        case "loadDiff":
          await this.loadDiff(message.threadId);
          return;
        case "applyThreadChanges": {
          const result = await this.withClient((client) =>
            client.applyThreadChanges(message.threadId),
          );
          this.applyResults.set(message.threadId, result);
          this.notice(
            reply,
            result.applied
              ? "Applied managed changes to the visible repository."
              : (result.blockers[0] ?? "No managed changes to apply."),
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
        case "deleteRepo":
          await this.deleteRepo(message.repoId);
          return;
        case "clearRepos":
          await this.clearRepos();
          return;
        case "setLimitPolicy":
          {
            const policy = normalizeLimitPolicy(message.policy);
            const applied = await this.withLocalClient((client) =>
              client.setLimitPolicy(policy),
            );
            await this.mirrorLimitPolicyToConfig(applied);
          }
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "setSandboxPolicy":
          {
            const applied = await this.withLocalClient((client) =>
              client.setSandboxPolicy(message.policy),
            );
            await this.mirrorSandboxPolicyToConfig(applied);
          }
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "setCloudPolicy":
          {
            const applied = await this.withLocalClient((client) =>
              client.setCloudPolicy(message.policy),
            );
          // Mirror into VS Code settings so the next settings sync doesn't undo
          // what the user just applied from the in-webview sheet.
            await this.mirrorCloudPolicyToConfig(applied);
          }
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "setLocalModelPolicy":
          {
            const applied = await this.withLocalClient((client) =>
              client.setLocalModelPolicy(
                normalizeLocalModelPolicy(message.policy),
              ),
            );
            await this.mirrorLocalModelPolicyToConfig(applied);
          }
          this.lastSyncedSettings = "";
          this.detectionCache = null;
          await this.refresh();
          return;
        case "sandboxLogin":
          await this.startSandboxLogin(!!message.codex, reply);
          return;
        case "signInAgent":
          await this.startAgentSignIn(message.agent, reply);
          return;
        case "githubSignIn":
          await this.githubToken();
          this.notice(reply, "GitHub sign-in is ready.");
          this.detectionCache = null;
          await this.refresh();
          return;
        case "refreshReadiness":
          this.detectionCache = null;
          await this.refresh();
          return;
        case "launchCloudHandoff": {
          const run = await this.withClient((client) =>
            client.launchCloudHandoff(message.threadId, message.agent ?? null),
          );
          this.notice(
            reply,
            `Started ${labelAgent(run.agent_kind)} cloud continuation.`,
          );
          await this.refresh();
          return;
        }
        case "reclaimCloudRun":
          await this.withClient((client) =>
            client.reclaimCloudRun(message.threadId),
          );
          this.notice(reply, "Reclaimed the cloud run.");
          await this.refresh();
          return;
        case "openPath":
          await vscode.env.openExternal(vscode.Uri.file(message.path));
          return;
        case "openExternal": {
          const uri = vscode.Uri.parse(message.url);
          if (uri.scheme !== "https" || !uri.authority) {
            throw new Error("Only HTTPS links can be opened from the workbench.");
          }
          await vscode.env.openExternal(uri);
          return;
        }
        case "openSettings":
          await vscode.commands.executeCommand(
            "workbench.action.openSettings",
            "@ext:SakethSripada.perpetual-for-vscode",
          );
          return;
        case "openPanel":
          await vscode.commands.executeCommand("perpetual.openWorkbench");
          return;
        case "deleteQueuedTurn":
          await this.withClient((client) =>
            client.deleteQueuedTurn(message.id),
          );
          await this.refresh();
          return;
        case "updateQueuedTurn":
          await this.withClient((client) =>
            client.updateQueuedTurn(message.id, message.message),
          );
          await this.refresh();
          return;
        case "reorderQueuedTurns":
          await this.withClient((client) =>
            client.reorderQueuedTurns(message.threadId, message.orderedIds),
          );
          await this.refresh();
          return;
        case "resolveApproval":
          await this.withClient((client) =>
            client.resolveApproval(message.id, message.decision),
          );
          await this.refresh();
          return;
        case "hostCollaboration":
        case "copyCollaborationInvite": {
          this.assertTrusted();
          const invite = await this.daemon.createCollaborationInvite();
          await vscode.env.clipboard.writeText(invite);
          reply?.({ type: "collaborationInvite", invite });
          this.notice(reply, "Encrypted invite copied. It expires in 15 minutes.");
          await this.refresh();
          return;
        }
        case "joinCollaboration":
          this.assertTrusted();
          await this.daemon.joinCollaboration(message.invite.trim());
          this.workspaceReposReady = null;
          this.detectionCache = null;
          this.notice(reply, "Connected to the shared workspace.");
          await this.refresh();
          return;
        case "leaveCollaboration": {
          const status = await this.daemon.collaborationStatus();
          if (status.role === "host") await this.daemon.stopCollaborationHost();
          else await this.daemon.leaveCollaboration();
          this.workspaceReposReady = null;
          this.notice(reply, status.role === "host" ? "Stopped sharing." : "Left the shared workspace.");
          await this.refresh();
          return;
        }
        case "revokeCollaborationDevice": {
          const confirm = await vscode.window.showWarningMessage(
            "Remove this device from the shared workspace?",
            { modal: true, detail: "Its encrypted credential is revoked immediately and active work is cancelled." },
            "Remove Device",
          );
          if (confirm !== "Remove Device") return;
          await this.daemon.revokeCollaborationDevice(message.deviceId);
          this.notice(reply, "Device access revoked.");
          await this.refresh();
          return;
        }
        case "cancelCollaborationAssignment":
          await this.withClient((client) =>
            client.cancelCollaborationAssignment(message.assignmentId),
          );
          this.notice(reply, "Stopped the device assignment.");
          await this.refresh();
          return;
        case "dismissCollaborationAssignmentIssue":
          await this.withClient((client) =>
            client.cancelCollaborationAssignment(message.assignmentId),
          );
          this.notice(reply, "Removed the device issue.");
          await this.refresh();
          return;
        case "retryCollaborationAssignment":
          await this.withClient((client) =>
            client.retryCollaborationAssignment(message.assignmentId),
          );
          this.notice(reply, "Device work queued again.");
          await this.refresh();
          return;
        case "addCollaborationRepoAndRetry":
          await this.addCollaborationRepoAndRetry(message.assignmentId, reply);
          return;
        case "applyCollaborationChangeSet": {
          if (message.overwrite) {
            const confirm = await vscode.window.showWarningMessage(
              "Replace overlapping local files with the device version?",
              {
                modal: true,
                detail:
                  "Perpetual keeps a recovery copy under its app data. Unrelated local files are not touched.",
              },
              "Overwrite & Keep Backup",
            );
            if (confirm !== "Overwrite & Keep Backup") return;
          }
          const change = await this.withClient((client) =>
            client.applyCollaborationChangeSet(message.changeSetId, !!message.overwrite),
          );
          this.notice(
            reply,
            change.status === "conflict"
              ? "Overlapping local edits found. Review them before choosing overwrite."
              : "Applied the device changes to the coordinator checkout.",
          );
          await this.refresh();
          return;
        }
        case "rejectCollaborationChangeSet":
          await this.withClient((client) =>
            client.rejectCollaborationChangeSet(message.changeSetId),
          );
          this.notice(reply, "Rejected the returned changes.");
          await this.refresh();
          return;
      }
    } catch (err) {
      const text = err instanceof Error ? err.message : String(err);
      this.output.appendLine(`[workbench] ${text}`);
      reply?.(
        message.type === "assignRepos"
          ? {
              type: "repoAssignmentFailed",
              threadId: message.threadId,
              message: text,
            }
          : { type: "error", message: text },
      );
      await this.refresh(text);
    }
  }

  private assignRepos(threadId: string, repoIds: string[]): Promise<void> {
    // Webview messages are delivered independently, so rapid checkbox clicks
    // can overlap. Keep only the newest queued value and drain one write at a
    // time; every caller waits until the final value has been persisted.
    this.repoAssignmentsPending.set(threadId, [...repoIds]);
    const inFlight = this.repoAssignmentsInFlight.get(threadId);
    if (inFlight) return inFlight;

    const run = this.drainRepoAssignments(threadId).finally(() => {
      this.repoAssignmentsInFlight.delete(threadId);
    });
    this.repoAssignmentsInFlight.set(threadId, run);
    return run;
  }

  private async drainRepoAssignments(threadId: string): Promise<void> {
    try {
      while (true) {
        const repoIds = this.repoAssignmentsPending.get(threadId);
        if (!repoIds) break;
        this.repoAssignmentsPending.delete(threadId);
        await this.withClient((client) =>
          client.assignThreadRepos(threadId, repoIds),
        );
      }
      this.diffCache.delete(threadId);
      await this.refresh();

      // A selection can arrive while refresh is yielding to the daemon. Drain
      // it before resolving the shared promise so no last click is stranded.
      if (this.repoAssignmentsPending.has(threadId)) {
        await this.drainRepoAssignments(threadId);
      }
    } catch (err) {
      this.repoAssignmentsPending.delete(threadId);
      throw err;
    }
  }

  async refresh(error: string | null = null): Promise<void> {
    const sequence = ++this.refreshSequence;
    const snapshot = await this.snapshot(error);
    if (!this.disposed && sequence === this.refreshSequence) {
      this.snapshots.fire(snapshot);
    }
  }

  async connectLocalRepoInteractive(reply?: WebviewReply): Promise<void> {
    this.assertTrusted();
    if ((await this.daemon.collaborationStatus()).role === "member") {
      throw new Error(
        "Repositories are managed by the shared-workspace host. Keep a matching clone open locally so this device can run assignments.",
      );
    }
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
    const repo = await client.connectLocalRepo({
      project_id: project.id,
      path: root,
    });
    reply?.({ type: "repoConnected", repo });
    await this.refresh();
  }

  private async addCollaborationRepoAndRetry(
    assignmentId: string,
    reply?: WebviewReply,
  ): Promise<void> {
    this.assertTrusted();
    const status = await this.daemon.collaborationStatus();
    const client = await this.daemon.getClient();
    const assignment = (await client.listCollaborationAssignments(null, false)).find(
      (item: CollaborationAssignment) => item.id === assignmentId,
    );
    if (!assignment) throw new Error("This device assignment no longer exists.");
    if (assignment.device_id !== status.deviceId) {
      throw new Error(`Open Perpetual on ${assignment.device_name} to add its repository clone.`);
    }

    const picked = await vscode.window.showOpenDialog({
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
      openLabel: "Add Clone and Retry",
      title: "Choose the Matching Repository Clone",
    });
    const folder = picked?.[0]?.fsPath;
    if (!folder) return;
    const root = await requiredGitRoot(folder);
    const alreadyOpen = (vscode.workspace.workspaceFolders ?? []).some(
      (workspaceFolder) => comparablePath(workspaceFolder.uri.fsPath) === comparablePath(root),
    );
    if (!alreadyOpen) {
      const added = vscode.workspace.updateWorkspaceFolders(
        vscode.workspace.workspaceFolders?.length ?? 0,
        0,
        { uri: vscode.Uri.file(root), name: path.basename(root) },
      );
      if (!added) {
        throw new Error(
          "VS Code could not add that clone to this workspace. Add it with File > Add Folder to Workspace, then retry.",
        );
      }
    }
    await client.retryCollaborationAssignment(assignmentId);
    this.notice(reply, `${path.basename(root)} added. Device work queued again.`);
    await this.refresh();
  }

  async connectGithubRepoInteractive(): Promise<void> {
    this.assertTrusted();
    const { repos } = await this.githubRepos();
    const items: Array<vscode.QuickPickItem & { repo: GithubRepository }> =
      repos.map((repo: GithubRepository) => ({
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
      return emptySnapshot(
        false,
        defaults,
        "Trust this workspace to connect repositories and run agent CLIs.",
      );
    }

    try {
      const client = await this.daemon.getClient();
      const localClient = await this.daemon.getLocalClient();
      const collaborationStatus = await this.daemon.collaborationStatus();
      await this.syncSettings(localClient);
      const project = await client.ensureWorkbenchProject();
      // Connecting workspace repos shells out to git and rarely changes mid-session;
      // do it once rather than on every event-driven refresh.
      if (collaborationStatus.role !== "member") {
        await this.ensureWorkspaceRepos(client, project.id);
      }

      // Fast path: threads/repos are cheap DB reads; agent + sandbox detection is
      // expensive (CLI probes) so it comes from a short-lived cache.
      const [allThreads, repos, detection] = await Promise.all([
        client.listAgentThreads(project.id),
        client.listRepos(project.id),
        this.detect(localClient),
      ]);
      const threads = filterAgentThreads(allThreads);
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
        localModelPolicy,
        state: detectionState,
      } = detection;
      for (const thread of threads) {
        this.maybeAutoApplyThread(thread);
      }

      const selectedThreadId = pickSelectedThread(
        this.context.workspaceState.get<string | null>(
          SELECTED_THREAD_KEY,
          null,
        ),
        threads,
      );
      if (
        selectedThreadId !==
        this.context.workspaceState.get(SELECTED_THREAD_KEY)
      ) {
        await this.context.workspaceState.update(
          SELECTED_THREAD_KEY,
          selectedThreadId,
        );
      }

      const [details, collaborationSnapshot] = await Promise.all([
        selectedThreadId
          ? loadThreadDetails(
              client,
              selectedThreadId,
              this.output,
              this.diffCache.get(selectedThreadId) ?? null,
              this.applyResults.get(selectedThreadId) ?? null,
            )
          : Promise.resolve(null),
        client.collaborationSnapshot(null, false),
      ]);

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
        localModelPolicy,
        detectionState,
        defaultRepoIds,
        limitPolicy,
        sandboxPolicy,
        sandboxRuntime,
        cloudPolicy,
        cloudAvailability,
        details,
        github: null,
        collaboration: {
          ...collaborationSnapshot,
          role: collaborationStatus.role,
          connected: collaborationStatus.connected,
          host_name: collaborationStatus.hostName,
          device_id: collaborationStatus.deviceId,
          device_name: collaborationStatus.deviceName,
        },
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
    const collaborationStatus = await this.daemon.collaborationStatus();
    // The user can change extension settings and submit immediately; mirror
    // the effective values before admitting this turn to the daemon.
    await this.syncSettings(await this.daemon.getLocalClient());
    const project = await client.ensureWorkbenchProject();
    if (collaborationStatus.role !== "member") {
      await this.ensureWorkspaceRepos(client, project.id);
    }
    const defaults = getDefaults();
    const agent = message.agent ?? defaults.agent;
    const permission = message.permission ?? defaults.permission;
    const executionBackend = sanitizeBackend(
      agent,
      message.executionBackend ?? defaults.execution_backend,
    );
    const localProvider =
      agent === "codex"
        ? sanitizeLocalProvider(
            message.localProvider ?? defaults.local_provider,
          )
        : null;
    const localBaseUrl = localProvider
      ? (blankToNull(message.localBaseUrl ?? defaults.local_base_url) ??
        defaultLocalBaseUrl(localProvider))
      : null;
    const rawModel = blankToNull(message.model ?? defaults.model);
    const model = sanitizeModelForAgent(agent, rawModel, localProvider);
    const reasoning = blankToNull(message.reasoning ?? defaults.reasoning);
    if (localProvider && !model) {
      throw new Error(
        "Choose a local model before running with Ollama or LM Studio.",
      );
    }

    const repos = await client.listRepos(project.id).catch(() => []);
    const repoIds = resolveSubmittedRepoIds(message.repoIds, repos);
    const targetDeviceId =
      message.deviceId ??
      (collaborationStatus.role === "member"
        ? collaborationStatus.deviceId
        : null);
    if (message.threadId) {
      const currentRepos = await client
        .listThreadRepos(message.threadId)
        .catch(() => []);
      const hasWorktree = currentRepos.some(
        (repo: { worktree_path: string | null }) => !!repo.worktree_path,
      );
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
        task_budget: message.taskBudget,
      });
      await this.selectThread(message.threadId);
      const activeAssignments = await client
        .listCollaborationAssignments(null, true)
        .catch(() => []);
      const activeRemote = activeAssignments.find(
        (assignment: CollaborationAssignment) => assignment.thread_id === message.threadId,
      );
      if (activeRemote) {
        void this.sendThreadMessageInBackground(
          message.threadId,
          agent,
          permission,
          text,
          blankToNull(message.clientMessageId ?? null),
        );
      } else if (targetDeviceId) {
        void this.createRemoteAssignmentInBackground(
          message.threadId,
          targetDeviceId,
          agent,
          permission,
          text,
          executionBackend,
          blankToNull(message.clientMessageId ?? null),
        );
      } else {
        void this.sendThreadMessageInBackground(
          message.threadId,
          agent,
          permission,
          text,
          blankToNull(message.clientMessageId ?? null),
        );
      }
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
      task_budget: message.taskBudget,
    });
    await this.selectThread(thread.id);
    // Do not make the first provider response wait for a full snapshot refresh
    // (agent detection, model catalogs, and workspace reads). The webview has
    // already rendered the optimistic message; the daemon events and the final
    // run refresh will fill in the new thread while the provider starts.
    if (targetDeviceId) {
      void this.createRemoteAssignmentInBackground(
        thread.id,
        targetDeviceId,
        agent,
        permission,
        text,
        executionBackend,
        blankToNull(message.clientMessageId ?? null),
      );
    } else {
      void this.runThreadInBackground(
        thread.id,
        agent,
        permission,
        text,
        executionBackend,
        blankToNull(message.clientMessageId ?? null),
      );
    }
    void this.refresh();
  }

  private async sendThreadMessageInBackground(
    threadId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    text: string,
    clientMessageId: string | null,
  ): Promise<void> {
    try {
      const client = await this.daemon.getClient();
      await client.sendThreadMessage(threadId, agent, permission, text, clientMessageId);
      await this.refresh();
    } catch (err) {
      const message = formatError(err);
      this.output.appendLine(`[workbench] ${message}`);
      await this.refresh(message);
    }
  }

  private async runThreadInBackground(
    threadId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    text: string,
    executionBackend: ExecutionBackend,
    clientMessageId: string | null,
  ): Promise<void> {
    try {
      const client = await this.daemon.getClient();
      await client.runAgentThread(
        threadId,
        agent,
        permission,
        text,
        executionBackend,
        clientMessageId,
      );
      await this.refresh();
    } catch (err) {
      const message = formatError(err);
      this.output.appendLine(`[workbench] ${message}`);
      await this.refresh(message);
    }
  }

  private async createRemoteAssignmentInBackground(
    threadId: string,
    deviceId: string,
    agent: AgentKind,
    permission: PermissionPolicy,
    text: string,
    executionBackend: ExecutionBackend,
    clientMessageId: string | null,
  ): Promise<void> {
    try {
      const client = await this.daemon.getClient();
      await client.createCollaborationAssignment({
        thread_id: threadId,
        device_id: deviceId,
        agent,
        permission,
        execution_backend: executionBackend,
        message: text,
        client_message_id: clientMessageId,
      });
      await this.refresh();
    } catch (err) {
      const message = formatError(err);
      this.output.appendLine(`[workbench] remote assignment failed: ${message}`);
      await this.refresh(message);
    }
  }

  private async connectWorkspaceRepos(): Promise<void> {
    this.assertTrusted();
    if ((await this.daemon.collaborationStatus()).role === "member") {
      throw new Error(
        "The host owns the shared repository list. Open matching local clones for device assignments.",
      );
    }
    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    this.autoConnectSuspended = false;
    await this.autoConnectWorkspaceRepos(client, project.id, true);
  }

  private async deleteRepo(repoId: string): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    await client.deleteRepo(repoId);
    // Otherwise the next auto-connect pass would immediately reconnect a
    // workspace folder the user just disconnected.
    this.autoConnectSuspended = true;
    await this.refresh();
  }

  private async clearRepos(): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    const project = await client.ensureWorkbenchProject();
    const repos = await client.listRepos(project.id).catch(() => []);
    if (repos.length === 0) return;
    const confirm = await vscode.window.showWarningMessage(
      `Disconnect all ${repos.length} connected ${repos.length === 1 ? "repository" : "repositories"}?`,
      {
        modal: true,
        detail:
          "Sessions keep their transcripts but lose their repository assignments. Nothing is deleted from disk.",
      },
      "Disconnect All",
    );
    if (confirm !== "Disconnect All") return;
    await client.clearProjectRepos(project.id);
    this.autoConnectSuspended = true;
    await this.refresh();
  }

  // Auto-connect must run at most once per workspace: `refresh()` and `submit()`
  // can overlap, and two concurrent runs both observe an empty repo list and
  // connect the same folder twice.
  private ensureWorkspaceRepos(
    client: DaemonApi,
    projectId: string,
  ): Promise<void> {
    if (this.autoConnectSuspended) return Promise.resolve();
    if (!this.workspaceReposReady) {
      this.workspaceReposReady = this.autoConnectWorkspaceRepos(
        client,
        projectId,
      ).catch((err) => {
        this.workspaceReposReady = null;
        throw err;
      });
    }
    return this.workspaceReposReady;
  }

  private async autoConnectWorkspaceRepos(
    client: DaemonApi,
    projectId: string,
    notifyErrors = false,
  ): Promise<void> {
    const folders = vscode.workspace.workspaceFolders ?? [];
    if (!folders.length) return;

    const existing = await client.listRepos(projectId).catch(() => []);
    const existingPaths = new Set(
      existing
        .map((repo) => repo.local_path)
        .filter((repoPath): repoPath is string => !!repoPath)
        .map((repoPath) => path.normalize(repoPath)),
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
          vscode.window.showWarningMessage(
            `Could not connect ${folder.name}: ${formatError(err)}`,
          );
        }
      }
    }
  }

  private async postGithubRepos(reply?: WebviewReply): Promise<void> {
    const { status, repos } = await this.githubRepos();
    reply?.({ type: "githubRepos", status, repos });
  }

  private async githubRepos(): Promise<{
    status: GithubAuthStatus;
    repos: GithubRepository[];
  }> {
    this.assertTrusted();
    const token = await this.githubToken();
    const client = await this.daemon.getClient();
    const [status, repos] = await Promise.all([
      client.githubAuthStatus(token),
      client.githubListRepositories(token),
    ]);
    return { status, repos };
  }

  private async connectGithubRepo(
    repo: GithubRepository,
    reply?: WebviewReply,
  ): Promise<void> {
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

  private async startSandboxLogin(
    codex: boolean,
    reply?: WebviewReply,
  ): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    const prompt = codex
      ? await client.codexSandboxLogin()
      : await client.sandboxLogin();
    const promptUri = vscode.Uri.parse(prompt.url);
    if (promptUri.scheme !== "https" || !promptUri.authority) {
      throw new Error("Sandbox sign-in returned an invalid HTTPS URL.");
    }
    reply?.({ type: "sandboxLoginPrompt", prompt, codex });
    await vscode.env.openExternal(promptUri);
  }

  private async startAgentSignIn(
    agent: AgentKind,
    reply?: WebviewReply,
  ): Promise<void> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    const statuses: AgentStatus[] = await client
      .detectAgents()
      .catch(() => []);
    const status = statuses.find((item) => item.kind === agent);
    const binary = status?.binary_path;
    if (!status?.installed || !binary) {
      this.notice(
        reply,
        `${labelAgent(agent)} CLI is not installed or could not be found.`,
      );
      return;
    }
    const terminal = vscode.window.createTerminal(`${labelAgent(agent)} Sign In`);
    terminal.show(true);
    terminal.sendText(signInCommand(agent, binary, vscode.env.shell), true);
    this.notice(reply, `Opened ${labelAgent(agent)} sign-in in a terminal.`);
  }

  private async loadDiff(threadId: string): Promise<void> {
    this.diffCache.set(threadId, { state: "loading", diff: null });
    await this.refresh();
    try {
      const diff = await this.withClient((client) =>
        client.threadDiff(threadId),
      );
      this.diffCache.set(threadId, { state: "ready", diff });
    } catch (err) {
      this.output.appendLine(
        `[workbench] threadDiff failed: ${formatError(err)}`,
      );
      this.diffCache.set(threadId, { state: "error", diff: null });
    }
    await this.refresh();
  }

  private async withClient<T>(
    fn: (client: DaemonApi) => Promise<T>,
  ): Promise<T> {
    this.assertTrusted();
    const client = await this.daemon.getClient();
    return fn(client);
  }

  private async withLocalClient<T>(
    fn: (client: DaemonApi) => Promise<T>,
  ): Promise<T> {
    this.assertTrusted();
    return fn(await this.daemon.getLocalClient());
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
        localModelPolicy,
      ] = await Promise.all([
        client
          .detectAgents()
          .then(filterAgentStatuses)
          .catch((err) => {
            this.output.appendLine(
              `[workbench] agent detection failed: ${formatError(err)}`,
            );
            return [];
          }),
        client
          .agentRunDefaults()
          .then(filterRunDefaults)
          .catch(() => []),
        client
          .agentModelCatalog()
          .then(filterModelCatalog)
          .catch(() => []),
        client.detectLocalModels().catch(() => []),
        client
          .getLimitPolicy()
          .then(normalizeLimitPolicy)
          .catch(() => null),
        client.getSandboxPolicy().catch(() => null),
        client.detectSandboxRuntime().catch(() => null),
        client
          .getCloudPolicy()
          .then(normalizeCloudPolicy)
          .catch(() => null),
        client
          .cloudAvailability()
          .then(filterCloudAvailability)
          .catch(() => []),
        client
          .getLocalModelPolicy()
          .then(normalizeLocalModelPolicy)
          .catch(() => null),
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
        localModelPolicy,
        state: "ready",
      };
      this.detectionCache = next;
      return next;
    })()
      .catch((err) => {
        this.output.appendLine(
          `[workbench] detection failed: ${formatError(err)}`,
        );
        const failed = emptyDetectionCache("error");
        this.detectionCache = failed;
        return failed;
      })
      .finally(() => {
        this.detectInflight = null;
      });
    return this.detectInflight;
  }

  private async syncSettings(client: DaemonApi): Promise<void> {
    const settings = getSettingsSnapshot();
    const encoded = JSON.stringify(settings);
    if (encoded === this.lastSyncedSettings) return;

    const [limitPolicy, sandboxPolicy, cloudPolicy, localModelPolicy] =
      await Promise.all([
        client.getLimitPolicy().catch(() => null),
        client.getSandboxPolicy().catch(() => null),
        client.getCloudPolicy().catch(() => null),
        client.getLocalModelPolicy().catch(() => null),
      ]);
    if (limitPolicy) {
      await client.setLimitPolicy(
        normalizeLimitPolicy({
          ...limitPolicy,
          auto_switch: settings.autoSwitchOnLimit,
          switch_back: settings.switchBackOnRecovery,
          resume_with_earliest: settings.resumeWithEarliestAgent,
          unknown_reset_retry_secs: settings.unknownLimitRetrySeconds,
          agent_priority: settings.fallbackPriority,
        }),
      );
    }
    if (cloudPolicy) {
      await client.setCloudPolicy(
        normalizeCloudPolicy({
          ...cloudPolicy,
          enabled: settings.cloudAutoCarryover,
          continue_on_sleep: settings.cloudCarryOverOnSleep,
          continue_on_shutdown: settings.cloudCarryOverOnShutdown,
          allow_cross_provider:
            settings.cloudProviderStrategy === "switch_provider",
          provider_priority: settings.cloudProviderPriority,
          require_approval: settings.cloudRequireApproval,
          max_concurrent_cloud_runs: settings.cloudMaxConcurrentRuns,
          codex_env_id: blankToNull(settings.cloudCodexEnvId),
        }),
      );
    }
    if (localModelPolicy) {
      await client.setLocalModelPolicy(
        normalizeLocalModelPolicy({
          ...localModelPolicy,
          auto_resume_cloud: settings.localAutoResumeCloud,
          use_local_fallback: settings.localUseFallback,
          switch_back_to_cloud: settings.localSwitchBackToCloud,
          probe_interval_secs: settings.localProbeIntervalSeconds,
          ollama_base_url:
            blankToNull(settings.localOllamaBaseUrl) ??
            localModelPolicy.ollama_base_url,
          lm_studio_base_url:
            blankToNull(settings.localLmStudioBaseUrl) ??
            localModelPolicy.lm_studio_base_url,
        }),
      );
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
    const config = vscode.workspace.getConfiguration("perpetual");
    const target = vscode.ConfigurationTarget.Global;
    await Promise.all([
      config.update("cloud.autoCarryover", policy.enabled, target),
      config.update("cloud.carryOverOnSleep", policy.continue_on_sleep, target),
      config.update(
        "cloud.carryOverOnShutdown",
        policy.continue_on_shutdown,
        target,
      ),
      config.update(
        "cloud.providerStrategy",
        policy.allow_cross_provider ? "switch_provider" : "same_provider",
        target,
      ),
      config.update(
        "cloud.providerPriority",
        normalizeAgentPriority(policy.provider_priority),
        target,
      ),
      config.update("cloud.requireApproval", policy.require_approval, target),
      config.update(
        "cloud.maxConcurrentRuns",
        policy.max_concurrent_cloud_runs,
        target,
      ),
      config.update("cloud.codexEnvId", policy.codex_env_id ?? "", target),
    ]);
  }

  private async mirrorLimitPolicyToConfig(policy: LimitPolicy): Promise<void> {
    const config = vscode.workspace.getConfiguration("perpetual");
    const target = vscode.ConfigurationTarget.Global;
    await Promise.all([
      config.update("autoSwitchOnLimit", policy.auto_switch, target),
      config.update("switchBackOnRecovery", policy.switch_back, target),
      config.update(
        "autoResumeOnLimitReset",
        policy.resume_with_earliest,
        target,
      ),
      config.update(
        "resumeWithEarliestAgent",
        policy.resume_with_earliest,
        target,
      ),
      config.update(
        "unknownLimitRetrySeconds",
        policy.unknown_reset_retry_secs,
        target,
      ),
      config.update(
        "fallbackPriority",
        normalizeAgentPriority(policy.agent_priority),
        target,
      ),
    ]);
  }

  private async mirrorSandboxPolicyToConfig(
    policy: SandboxPolicy,
  ): Promise<void> {
    const config = vscode.workspace.getConfiguration("perpetual");
    const target = vscode.ConfigurationTarget.Global;
    await Promise.all([
      config.update("defaultExecutionBackend", policy.default_backend, target),
      config.update(
        "sandbox.maxConcurrent",
        policy.max_concurrent_sandboxes,
        target,
      ),
      config.update("sandbox.cpus", policy.cpus, target),
      config.update("sandbox.memory", policy.memory, target),
      config.update("sandbox.networkPreset", policy.network_preset, target),
    ]);
  }

  private async mirrorLocalModelPolicyToConfig(
    policy: LocalModelPolicy,
  ): Promise<void> {
    const config = vscode.workspace.getConfiguration("perpetual");
    const target = vscode.ConfigurationTarget.Global;
    await Promise.all([
      config.update("local.autoResumeCloud", policy.auto_resume_cloud, target),
      config.update("local.useFallback", policy.use_local_fallback, target),
      config.update(
        "local.switchBackToCloud",
        policy.switch_back_to_cloud,
        target,
      ),
      config.update(
        "local.probeIntervalSeconds",
        policy.probe_interval_secs,
        target,
      ),
      config.update("local.ollamaBaseUrl", policy.ollama_base_url, target),
      config.update("local.lmStudioBaseUrl", policy.lm_studio_base_url, target),
    ]);
  }

  private async selectThread(threadId: string | null): Promise<void> {
    await this.context.workspaceState.update(SELECTED_THREAD_KEY, threadId);
  }

  private onDaemonEvent(event: AppEvent): void {
    if (event.type === "event_gap") {
      this.detectionCache = null;
      void this.refresh();
      return;
    }
    if (event.type === "agent_thread_event") {
      const data = event.data as AgentThreadEvent;
      this.threadEvents.fire(data);
      if (data.thread_id) this.diffCache.delete(data.thread_id);
    }
    if (event.type === "agent_thread_updated") {
      const data = event.data as Partial<AgentThread>;
      if (data.id && data.status === "running") {
        this.diffCache.delete(data.id);
        this.applyResults.delete(data.id);
        this.autoAppliedThreads.delete(data.id);
      }
      this.maybeAutoApplyThread(data);
    }
    if (event.type === "cloud_run_updated") {
      const data = event.data as { thread_id?: string };
      if (data.thread_id) this.diffCache.delete(data.thread_id);
    }
    if (event.type === "provider_usage_updated") {
      const data = event.data as { agent: AgentKind; usage: ProviderUsage };
      if (this.detectionCache) {
        this.detectionCache = {
          ...this.detectionCache,
          agents: this.detectionCache.agents.map((status) =>
            status.kind === data.agent ? { ...status, usage: data.usage } : status,
          ),
        };
      }
    }
    // Approvals are interactive — surface them immediately. Everything else is
    // throttled (not debounced): a continuous token stream must keep painting
    // instead of postponing the refresh until the provider pauses.
    const immediate =
      event.type === "approval_requested" || event.type === "approval_resolved";
    if (immediate && this.refreshTimer) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
    if (immediate) {
      void this.refresh();
      return;
    }
    if (this.refreshTimer) return;
    // Transcript events already travel directly to the webview; this slower
    // snapshot cadence reconciles durable state without turning each token into
    // a bundle of database RPCs.
    const delay = event.type === "agent_thread_event" ? 120 : 80;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      void this.refresh();
    }, delay);
  }

  private maybeAutoApplyThread(thread: Partial<AgentThread>): void {
    if (
      thread.id &&
      thread.status === "review" &&
      thread.permission &&
      thread.permission !== "read_only" &&
      !this.autoAppliedThreads.has(thread.id)
    ) {
      this.autoAppliedThreads.add(thread.id);
      void this.autoApplyThreadChanges(thread.id);
    }
  }

  private async autoApplyThreadChanges(threadId: string): Promise<void> {
    if (this.autoApplyInFlight.has(threadId)) return;
    if (!this.autoAppliedThreads.has(threadId)) return;
    this.autoApplyInFlight.add(threadId);
    try {
      const result = await this.withClient(async (client) => {
        const repos = await client.listThreadRepos(threadId).catch(() => []);
        const hasManagedWorkspace = repos.some(
          (repo) =>
            Boolean(repo.worktree_path && repo.branch?.startsWith("am/thread-")),
        );
        if (!hasManagedWorkspace) return null;
        return client.applyThreadChanges(threadId);
      });
      if (!result) return;
      this.applyResults.set(threadId, result);
      this.diffCache.delete(threadId);
      if (result.applied) {
        this.output.appendLine(
          `[workbench] auto-applied managed changes for thread ${threadId}`,
        );
      } else if (result.blockers.length) {
        this.output.appendLine(
          `[workbench] auto-apply blocked for thread ${threadId}: ${result.blockers.join("; ")}`,
        );
      }
    } catch (err) {
      this.output.appendLine(
        `[workbench] auto-apply failed for thread ${threadId}: ${formatError(err)}`,
      );
    } finally {
      this.autoApplyInFlight.delete(threadId);
      await this.refresh();
    }
  }

  private notice(reply: WebviewReply | undefined, message: string): void {
    reply?.({ type: "notice", message });
  }

  private assertTrusted(): void {
    if (!vscode.workspace.isTrusted) {
      throw new Error(
        "Trust this workspace before connecting repositories or running agents.",
      );
    }
  }
}

async function loadThreadDetails(
  client: DaemonApi,
  threadId: string,
  output: vscode.OutputChannel,
  diffEntry: DiffCacheEntry | null,
  applyResult: AgentThreadApplyResult | null,
): Promise<ThreadDetails> {
  const [events, activities, repos, turns, queued, cloudRuns, approvals] =
    await Promise.all([
    client.listThreadEvents(threadId).catch((err) => {
      output.appendLine(
        `[workbench] listThreadEvents failed: ${formatError(err)}`,
      );
      return [];
    }),
    client
      .listActivity(null, 250)
      .then((items) => items.filter((item) => activityBelongsToThread(item, threadId)))
      .catch((err) => {
        output.appendLine(
          `[workbench] listActivity failed: ${formatError(err)}`,
        );
        return [];
      }),
    client.listThreadRepos(threadId).catch(() => []),
    client.listThreadTurns(threadId).catch(() => []),
    client.listQueuedTurns(threadId).catch(() => []),
    client.listCloudRuns(threadId).catch(() => []),
    client.listPendingApprovals().catch(() => []),
  ]);
  return {
    events,
    activities,
    repos,
    turns,
    queued,
    cloudRuns,
    diff: diffEntry?.diff ?? null,
    diffState: diffEntry?.state ?? "idle",
    applyResult,
    approvals: approvals.filter((approval) => approval.thread_id === threadId),
  };
}

function activityBelongsToThread(
  activity: { payload: unknown },
  threadId: string,
): boolean {
  const payload = activity.payload;
  return (
    !!payload &&
    typeof payload === "object" &&
    (payload as { thread_id?: unknown }).thread_id === threadId
  );
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
    localModelPolicy: null,
    state,
  };
}

function emptySnapshot(
  trusted: boolean,
  defaults: WorkbenchDefaults,
  error: string,
): WorkbenchSnapshot {
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
    localModelPolicy: null,
    detectionState: "idle",
    defaultRepoIds: [],
    limitPolicy: null,
    sandboxPolicy: null,
    sandboxRuntime: null,
    cloudPolicy: null,
    cloudAvailability: [],
    details: null,
      github: null,
      collaboration: {
        role: "standalone",
        connected: false,
        host_name: null,
        device_id: "",
        device_name: "This device",
        devices: [],
        assignments: [],
        change_sets: [],
        server_time: new Date().toISOString(),
      },
      error,
  };
}

function pickSelectedThread(
  selected: string | null,
  threads: AgentThread[],
): string | null {
  if (selected && threads.some((thread) => thread.id === selected))
    return selected;
  return threads[0]?.id ?? null;
}

function pickDefaultRepoIds(
  repos: Array<{ id: string; local_path: string | null }>,
): string[] {
  const workspacePaths = (vscode.workspace.workspaceFolders ?? []).map((folder) =>
    comparablePath(folder.uri.fsPath),
  );
  const activePath = vscode.window.activeTextEditor?.document.uri.fsPath;
  const contextPaths = workspacePaths.length
    ? workspacePaths
    : activePath
      ? [comparablePath(activePath)]
      : [];
  if (contextPaths.length === 0 && repos.length === 1) return [repos[0].id];

  const matches = repos.filter((repo) => {
    if (!repo.local_path) return false;
    const root = comparablePath(repo.local_path);
    return contextPaths.some(
      (contextPath) =>
        isWithinPath(root, contextPath) || isWithinPath(contextPath, root),
    );
  });
  return matches.map((repo) => repo.id);
}

function comparablePath(value: string): string {
  const normalized = path.normalize(value);
  return process.platform === "win32" ? normalized.toLowerCase() : normalized;
}

function isWithinPath(parent: string, child: string): boolean {
  return parent === child || child.startsWith(`${parent}${path.sep}`);
}

function resolveSubmittedRepoIds(
  submitted: string[] | undefined,
  repos: Array<{ id: string; local_path: string | null }>,
): string[] {
  const known = new Set(repos.map((repo) => repo.id));
  const picked =
    submitted === undefined ? pickDefaultRepoIds(repos) : submitted;
  const repoIds = Array.from(new Set(picked)).filter((id) => known.has(id));
  if (repos.length > 0 && repoIds.length === 0) {
    throw new Error(
      "Select at least one connected repository before starting the agent.",
    );
  }
  return repoIds;
}

async function gitRoot(folder: string): Promise<string> {
  try {
    const { stdout } = await execFileAsync("git", [
      "-C",
      folder,
      "rev-parse",
      "--show-toplevel",
    ]);
    return stdout.trim() || folder;
  } catch {
    return folder;
  }
}

async function requiredGitRoot(folder: string): Promise<string> {
  try {
    const { stdout } = await execFileAsync("git", [
      "-C",
      folder,
      "rev-parse",
      "--show-toplevel",
    ]);
    const root = stdout.trim();
    if (root) return root;
  } catch {
    // Use the actionable message below for non-repositories and inaccessible folders.
  }
  throw new Error("That folder is not a Git repository. Choose a clone of the repository used by this session.");
}

function titleFromMessage(message: string): string {
  const singleLine = message.replace(/\s+/g, " ").trim();
  return singleLine.length > 56
    ? `${singleLine.slice(0, 53)}...`
    : singleLine || "New session";
}

function getDefaults(): WorkbenchDefaults {
  const config = vscode.workspace.getConfiguration("perpetual");
  return {
    agent: sanitizeAgent(config.get<string>("defaultAgent", "claude_code")),
    permission: config.get<PermissionPolicy>(
      "defaultPermission",
      "workspace_write",
    ),
    execution_backend: config.get<ExecutionBackend>(
      "defaultExecutionBackend",
      "host",
    ),
    model: blankToNull(config.get<string>("defaultModel", "")),
    reasoning: blankToNull(config.get<string>("defaultReasoning", "medium")),
    local_provider: sanitizeLocalProvider(
      config.get<string>("defaultLocalProvider", ""),
    ),
    local_base_url: blankToNull(config.get<string>("defaultLocalBaseUrl", "")),
  };
}

function getSettingsSnapshot() {
  const config = vscode.workspace.getConfiguration("perpetual");
  const autoResumeSetting = config.inspect<boolean>("autoResumeOnLimitReset");
  const autoResumeIsExplicit = Boolean(
    autoResumeSetting?.globalValue !== undefined ||
      autoResumeSetting?.workspaceValue !== undefined ||
      autoResumeSetting?.workspaceFolderValue !== undefined,
  );
  return {
    defaultExecutionBackend: config.get<ExecutionBackend>(
      "defaultExecutionBackend",
      "host",
    ),
    autoSwitchOnLimit: config.get<boolean>("autoSwitchOnLimit", true),
    switchBackOnRecovery: config.get<boolean>("switchBackOnRecovery", true),
    resumeWithEarliestAgent:
      autoResumeIsExplicit
        ? config.get<boolean>("autoResumeOnLimitReset", true)
        : config.get<boolean>("resumeWithEarliestAgent", true),
    unknownLimitRetrySeconds: config.get<number>(
      "unknownLimitRetrySeconds",
      600,
    ),
    fallbackPriority: normalizeAgentPriority(
      config.get<AgentKind[]>("fallbackPriority", ["claude_code", "codex"]),
    ),
    cloudAutoCarryover: config.get<boolean>("cloud.autoCarryover", false),
    cloudCarryOverOnSleep: config.get<boolean>("cloud.carryOverOnSleep", true),
    cloudCarryOverOnShutdown: config.get<boolean>(
      "cloud.carryOverOnShutdown",
      true,
    ),
    cloudProviderStrategy: config.get<string>(
      "cloud.providerStrategy",
      "same_provider",
    ),
    cloudProviderPriority: normalizeAgentPriority(
      config.get<AgentKind[]>("cloud.providerPriority", [
        "claude_code",
        "codex",
      ]),
    ),
    cloudRequireApproval: config.get<boolean>("cloud.requireApproval", false),
    cloudMaxConcurrentRuns: config.get<number>("cloud.maxConcurrentRuns", 2),
    cloudCodexEnvId: config.get<string>("cloud.codexEnvId", ""),
    localAutoResumeCloud: config.get<boolean>("local.autoResumeCloud", true),
    localUseFallback: config.get<boolean>("local.useFallback", true),
    localSwitchBackToCloud: config.get<boolean>(
      "local.switchBackToCloud",
      true,
    ),
    localProbeIntervalSeconds: config.get<number>(
      "local.probeIntervalSeconds",
      30,
    ),
    localOllamaBaseUrl: config.get<string>("local.ollamaBaseUrl", ""),
    localLmStudioBaseUrl: config.get<string>("local.lmStudioBaseUrl", ""),
    sandboxMaxConcurrent: config.get<number>("sandbox.maxConcurrent", 2),
    sandboxCpus: config.get<number>("sandbox.cpus", 2),
    sandboxMemory: config.get<string>("sandbox.memory", "4g"),
    sandboxNetworkPreset: config.get<string>(
      "sandbox.networkPreset",
      "balanced",
    ),
  };
}

function sanitizeBackend(
  agent: AgentKind,
  backend: ExecutionBackend,
): ExecutionBackend {
  if (backend === "docker_sandbox" && agent !== "codex") {
    return "host";
  }
  return backend;
}

function sanitizeAgent(value: string | null | undefined): AgentKind {
  return value === "codex" ? "codex" : "claude_code";
}

function labelAgent(agent: AgentKind | null | undefined): string {
  if (agent === "codex") return "Codex";
  if (agent === "claude_code") return "Claude";
  return "Agent";
}

function isSupportedAgent(
  value: string | null | undefined,
): value is AgentKind {
  return value === "claude_code" || value === "codex";
}

function normalizeAgentPriority(
  value: readonly AgentKind[] | null | undefined,
): AgentKind[] {
  const out: AgentKind[] = [];
  for (const agent of value ?? []) {
    if (isSupportedAgent(agent) && !out.includes(agent)) out.push(agent);
  }
  for (const agent of ["claude_code", "codex"] as const) {
    if (!out.includes(agent)) out.push(agent);
  }
  return out;
}

function filterAgentStatuses(items: AgentStatus[]): AgentStatus[] {
  return items.filter((item) => isSupportedAgent(item.kind));
}

function filterRunDefaults(items: AgentRunDefaults[]): AgentRunDefaults[] {
  return items.filter((item) => isSupportedAgent(item.kind));
}

function filterModelCatalog(items: AgentModelCatalog[]): AgentModelCatalog[] {
  return items.filter((item) => isSupportedAgent(item.agent));
}

function filterCloudAvailability(
  items: CloudAvailability[],
): CloudAvailability[] {
  return items.filter((item) => isSupportedAgent(item.agent));
}

function filterAgentThreads(items: AgentThread[]): AgentThread[] {
  return items.filter((item) => {
    const active = item.active_agent ?? item.preferred_agent;
    return !active || isSupportedAgent(active);
  });
}

function normalizeLimitPolicy(policy: LimitPolicy): LimitPolicy {
  return {
    ...policy,
    agent_priority: normalizeAgentPriority(policy.agent_priority),
    unknown_reset_retry_secs: clampInt(
      policy.unknown_reset_retry_secs,
      0,
      7 * 24 * 60 * 60,
    ),
  };
}

function normalizeCloudPolicy(policy: CloudPolicy): CloudPolicy {
  return {
    ...policy,
    provider_priority: normalizeAgentPriority(policy.provider_priority),
    max_concurrent_cloud_runs: clampInt(
      policy.max_concurrent_cloud_runs,
      1,
      8,
    ),
  };
}

function normalizeLocalModelPolicy(policy: LocalModelPolicy): LocalModelPolicy {
  return {
    ...policy,
    probe_interval_secs: clampInt(policy.probe_interval_secs, 5, 3600),
    offline_grace_secs: clampInt(policy.offline_grace_secs, 0, 3600),
    stable_successes: clampInt(policy.stable_successes, 1, 20),
    targets: policy.targets.filter((target) => !!target.model.trim()),
  };
}

function clampInt(value: number, min: number, max: number): number {
  const n = Number.isFinite(value) ? Math.trunc(value) : min;
  return Math.min(max, Math.max(min, n));
}

function blankToNull(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

function sanitizeLocalProvider(
  value: string | null | undefined,
): LocalModelProvider | null {
  return value === "ollama" || value === "lm_studio" ? value : null;
}

function sanitizeModelForAgent(
  _agent: AgentKind,
  model: string | null,
  localProvider: LocalModelProvider | null,
): string | null {
  if (!model) return null;
  return model;
}

function defaultLocalBaseUrl(provider: LocalModelProvider): string {
  return provider === "lm_studio"
    ? "http://127.0.0.1:1234"
    : "http://127.0.0.1:11434";
}

function formatError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function isPowerShell(shell: string | undefined): boolean {
  const name = (shell ?? "")
    .toLowerCase()
    .replace(/\\/g, "/")
    .split("/")
    .pop();
  return (
    name === "powershell.exe" ||
    name === "pwsh.exe" ||
    name === "powershell" ||
    name === "pwsh"
  );
}

/**
 * The sign-in command as the terminal's own shell will parse it. The shell is
 * whatever profile VS Code launches (`vscode.env.shell`), not whatever the
 * platform implies: a Windows user may well be sitting in git bash.
 */
export function signInCommand(
  agent: AgentKind,
  binary: string,
  shell: string | undefined,
): string {
  const powershell = isPowerShell(shell);
  const quoted = shellQuote(binary, powershell);
  // PowerShell parses a leading quoted string as a string literal, so an
  // executable path only runs when handed to the call operator.
  const run = (args: string) =>
    powershell ? `& ${quoted} ${args}` : `${quoted} ${args}`;

  if (agent === "codex") {
    return run("login");
  }
  // Windows PowerShell 5.1 has no `||` operator, so branch on the exit code.
  return powershell
    ? `${run("auth login")}; if ($LASTEXITCODE -ne 0) { ${run("login")} }`
    : `${run("auth login")} || ${run("login")}`;
}

function shellQuote(value: string, powershell: boolean): string {
  if (powershell) {
    return `'${value.replace(/'/g, "''")}'`;
  }
  if (process.platform === "win32") {
    return `"${value.replace(/"/g, '\\"')}"`;
  }
  return `'${value.replace(/'/g, "'\\''")}'`;
}
