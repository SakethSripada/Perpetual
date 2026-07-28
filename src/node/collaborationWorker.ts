import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as vscode from "vscode";
import type { DaemonClient } from "./daemonClient";
import type {
  AgentThread,
  AgentThreadEvent,
  AgentThreadRepo,
  AppEvent,
  ApprovalRequest,
  ClaimedCollaborationAssignment,
  CollaborationAssignment,
  CollaborationChangeSet,
  QueuedTurn,
  Repo,
} from "./types";

const execFileAsync = promisify(execFile);
const POLL_MS = 2_500;
const LEASE_RENEW_MS = 15_000;
const STREAM_FLUSH_MS = 250;
const MAX_PARALLEL_ASSIGNMENTS = 4;
const MAX_PATCH_BYTES = 16 * 1024 * 1024;

type ActiveRemoteRun = {
  claimed: ClaimedCollaborationAssignment;
  localThreadId: string;
  repoMap: Map<string, string>; // local repo id -> coordinator repo id
  renewTimer: NodeJS.Timeout;
  renewFailures: number;
  finishing: boolean;
  pendingEvents: Map<string, AgentThreadEvent>;
  flushTimer: NodeJS.Timeout | null;
  continuationExpected: boolean;
  deliveredInstructions: Set<string>;
  reportedApprovals: Set<string>;
};

/**
 * Bridges coordinator assignments to this installation's locally authenticated
 * CLIs. Provider credentials and provider-native session ids never cross the
 * network; only bounded prompts, normalized events, and reviewable patches do.
 */
export class CollaborationWorker implements vscode.Disposable {
  private readonly active = new Map<string, ActiveRemoteRun>();
  private readonly localThreadToAssignment = new Map<string, string>();
  private pollTimer: NodeJS.Timeout | null = null;
  private polling = false;
  private disposed = false;

  constructor(
    private readonly coordinator: DaemonClient,
    private readonly local: DaemonClient,
    private readonly deviceId: string,
    private readonly output: vscode.OutputChannel,
  ) {
    local.on("event", this.onLocalEvent);
  }

  start(): void {
    if (this.pollTimer || this.disposed) return;
    void this.poll();
    this.pollTimer = setInterval(() => void this.poll(), POLL_MS);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
    this.local.off("event", this.onLocalEvent);
    for (const run of this.active.values()) {
      clearInterval(run.renewTimer);
      if (run.flushTimer) clearTimeout(run.flushTimer);
      // A disconnected coordinator can no longer renew the fencing lease.
      // Stop promptly so local provider usage is not wasted on stale work.
      void this.local.stopAgentThread(run.localThreadId).catch(() => undefined);
    }
    this.active.clear();
    this.localThreadToAssignment.clear();
  }

  private poll = async (): Promise<void> => {
    if (this.polling || this.disposed) return;
    this.polling = true;
    try {
      const assignments: CollaborationAssignment[] =
        await this.coordinator.listCollaborationAssignments(this.deviceId, true);
      const visible = new Set(assignments.map((assignment) => assignment.id));
      for (const [assignmentId, run] of this.active) {
        if (!visible.has(assignmentId)) {
          await this.local.stopAgentThread(run.localThreadId).catch(() => undefined);
        }
      }
      const available = Math.max(0, MAX_PARALLEL_ASSIGNMENTS - this.active.size);
      const queued = assignments
        .filter((assignment) => assignment.status === "queued")
        .slice(0, available);
      await Promise.all(queued.map((assignment) => this.claimAndRun(assignment)));
      await Promise.all(
        [...this.active.values()].map(async (run) => {
          await Promise.all([
            this.syncInstructions(run),
            this.syncApprovalDecisions(run),
          ]);
        }),
      );
    } catch (error) {
      this.output.appendLine(`[collaboration-worker] poll delayed: ${formatError(error)}`);
    } finally {
      this.polling = false;
    }
  };

  private async claimAndRun(assignment: CollaborationAssignment): Promise<void> {
    if (this.active.has(assignment.id) || this.disposed) return;
    let claimed: ClaimedCollaborationAssignment;
    try {
      claimed = await this.coordinator.claimCollaborationAssignment(
        assignment.id,
        this.deviceId,
      );
    } catch {
      return; // another process/refresh won the atomic claim
    }

    let localThreadId: string | null = null;
    const preparationRenewTimer = setInterval(
      () =>
        void this.coordinator
          .renewCollaborationLease(
            claimed.assignment.id,
            claimed.lease_token,
          )
          .catch(() => undefined),
      LEASE_RENEW_MS,
    );
    try {
      const prepared = await this.prepareMirror(claimed.assignment);
      clearInterval(preparationRenewTimer);
      localThreadId = prepared.thread.id;
      const run: ActiveRemoteRun = {
        claimed,
        localThreadId,
        repoMap: prepared.repoMap,
        renewTimer: setInterval(
          () => void this.renewLease(claimed.assignment.id),
          LEASE_RENEW_MS,
        ),
        renewFailures: 0,
        finishing: false,
        pendingEvents: new Map(),
        flushTimer: null,
        continuationExpected: false,
        deliveredInstructions: new Set(),
        reportedApprovals: new Set(),
      };
      this.active.set(claimed.assignment.id, run);
      this.localThreadToAssignment.set(localThreadId, claimed.assignment.id);
      await this.local.runAgentThread(
        localThreadId,
        claimed.assignment.agent,
        claimed.assignment.permission,
        null,
        claimed.assignment.execution_backend,
        null,
      );
      this.output.appendLine(
        `[collaboration-worker] ${claimed.assignment.agent} started assignment ${claimed.assignment.id}`,
      );
    } catch (error) {
      clearInterval(preparationRenewTimer);
      if (localThreadId) {
        await this.local.deleteAgentThread(localThreadId, true).catch(() => undefined);
      }
      await this.coordinator
        .finishCollaborationAssignment({
          assignment_id: claimed.assignment.id,
          lease_token: claimed.lease_token,
          state: "failed",
          error: formatError(error),
        })
        .catch(() => undefined);
      this.output.appendLine(
        `[collaboration-worker] assignment ${claimed.assignment.id} failed to start: ${formatError(error)}`,
      );
    }
  }

  private async prepareMirror(assignment: CollaborationAssignment): Promise<{
    thread: AgentThread;
    repoMap: Map<string, string>;
  }> {
    const centralThread = await this.coordinator.getAgentThread(assignment.thread_id);
    if (!centralThread) throw new Error("The shared session no longer exists.");
    const [centralBindings, centralRepos]: [AgentThreadRepo[], Repo[]] = await Promise.all([
      this.coordinator.listThreadRepos(assignment.thread_id),
      centralThread.project_id
        ? this.coordinator.listRepos(centralThread.project_id)
        : Promise.resolve([] as Repo[]),
    ]);

    const localProject = await this.local.ensureWorkbenchProject();
    await this.connectWorkspaceRepositories(localProject.id);
    const localRepos: Repo[] = await this.local.listRepos(localProject.id);
    const mapping = mapRepositories(centralBindings, centralRepos, localRepos);
    if (centralBindings.length > 0 && mapping.repoIds.length !== centralBindings.length) {
      const missing = centralBindings
        .filter((binding) => !mapping.centralToLocal.has(binding.repo_id))
        .map((binding) => binding.repo_name)
        .join(", ");
      throw new Error(
        `Connect matching local repositories on this device before running shared work: ${missing}`,
      );
    }

    const thread = await this.local.createAgentThread({
      project_id: localProject.id,
      title: `Shared · ${centralThread.title}`,
      objective: assignment.prompt,
      repo_ids: mapping.repoIds,
      preferred_agent: assignment.agent,
      permission: assignment.permission,
      execution_backend: assignment.execution_backend,
      force_managed_workspace: true,
      model: centralThread.model,
      reasoning: centralThread.reasoning,
      local_provider: centralThread.local_provider,
      local_base_url: centralThread.local_base_url,
      task_budget: centralThread.task_budget,
    });

    // Bring already-approved peer patches into the new isolated worktree. This
    // transfers state without injecting transcript history into the model.
    const snapshot = await this.coordinator.collaborationSnapshot(
      assignment.thread_id,
      true,
    ) as { change_sets: CollaborationChangeSet[] };
    const approved = snapshot.change_sets
      .filter((change) =>
        change.status === "applied" || change.status === "applied_with_overwrite"
      )
      .reverse();
    for (const change of approved) {
      const localRepoId = mapping.centralToLocal.get(change.repo_id);
      if (localRepoId) {
        await this.local.importCollaborationPatch(thread.id, localRepoId, change.patch);
      }
    }

    return { thread, repoMap: mapping.localToCentral };
  }

  private async connectWorkspaceRepositories(projectId: string): Promise<void> {
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      try {
        const { stdout } = await execFileAsync(
          "git",
          ["-C", folder.uri.fsPath, "rev-parse", "--show-toplevel"],
          { timeout: 5_000 },
        );
        const root = stdout.trim();
        if (root) await this.local.connectLocalRepo({ project_id: projectId, path: root });
      } catch {
        // Non-git folders are harmless and should not block other mappings.
      }
    }
  }

  private renewLease = async (assignmentId: string): Promise<void> => {
    const run = this.active.get(assignmentId);
    if (!run || run.finishing || this.disposed) return;
    try {
      await this.coordinator.renewCollaborationLease(
        assignmentId,
        run.claimed.lease_token,
      );
      run.renewFailures = 0;
    } catch (error) {
      run.renewFailures += 1;
      if (run.renewFailures >= 2) {
        this.output.appendLine(
          `[collaboration-worker] lease lost for ${assignmentId}; stopping isolated run`,
        );
        await this.local.stopAgentThread(run.localThreadId).catch(() => undefined);
      } else {
        this.output.appendLine(
          `[collaboration-worker] lease renewal delayed: ${formatError(error)}`,
        );
      }
    }
  };

  private onLocalEvent = (event: AppEvent): void => {
    if (event.type === "approval_requested") {
      const approval = event.data as ApprovalRequest;
      const localThreadId = approval.thread_id;
      const assignmentId = localThreadId
        ? this.localThreadToAssignment.get(localThreadId)
        : null;
      const run = assignmentId ? this.active.get(assignmentId) : null;
      if (run) void this.reportApproval(run, approval);
      return;
    }
    if (event.type === "agent_thread_event") {
      const localEvent = event.data as AgentThreadEvent;
      const assignmentId = this.localThreadToAssignment.get(localEvent.thread_id);
      const run = assignmentId ? this.active.get(assignmentId) : null;
      if (!run || localEvent.role === "user") return;
      if (localEvent.kind === "assistant_text" && isStreaming(localEvent)) {
        run.pendingEvents.set(localEvent.id, localEvent);
        if (!run.flushTimer) {
          run.flushTimer = setTimeout(() => {
            run.flushTimer = null;
            void this.flushEvents(run);
          }, STREAM_FLUSH_MS);
        }
      } else {
        void this.reportEvent(run, localEvent);
      }
      return;
    }
    if (event.type === "agent_thread_updated") {
      const thread = event.data as AgentThread;
      const assignmentId = this.localThreadToAssignment.get(thread.id);
      const run = assignmentId ? this.active.get(assignmentId) : null;
      if (!run || run.finishing) return;
      if (thread.status === "running" && run.continuationExpected) {
        run.continuationExpected = false;
      }
      if (["review", "done", "failed", "paused", "cancelled"].includes(thread.status)) {
        run.finishing = true;
        void this.finishRun(run, thread.status);
      }
    }
  };

  private async flushEvents(run: ActiveRemoteRun): Promise<void> {
    const events = [...run.pendingEvents.values()];
    run.pendingEvents.clear();
    for (const event of events) await this.reportEvent(run, event);
  }

  private async reportEvent(run: ActiveRemoteRun, event: AgentThreadEvent): Promise<void> {
    try {
      await this.coordinator.reportCollaborationEvent({
        assignment_id: run.claimed.assignment.id,
        lease_token: run.claimed.lease_token,
        event_id: event.id,
        role: event.role,
        kind: event.kind,
        text: event.text,
        client_message_id: event.client_message_id,
        data: event.data,
        ts: event.ts,
      });
    } catch (error) {
      this.output.appendLine(
        `[collaboration-worker] event delivery delayed: ${formatError(error)}`,
      );
    }
  }

  private async reportApproval(
    run: ActiveRemoteRun,
    approval: ApprovalRequest,
  ): Promise<void> {
    try {
      await this.coordinator.reportCollaborationApproval({
        assignment_id: run.claimed.assignment.id,
        lease_token: run.claimed.lease_token,
        approval: sanitizeApprovalForCoordinator(approval),
      });
      run.reportedApprovals.add(approval.id);
    } catch (error) {
      this.output.appendLine(
        `[collaboration-worker] approval delivery delayed: ${formatError(error)}`,
      );
    }
  }

  private async finishRun(run: ActiveRemoteRun, status: string): Promise<void> {
    try {
      await this.syncInstructions(run);
    } catch (error) {
      run.finishing = false;
      this.output.appendLine(
        `[collaboration-worker] final instruction check delayed: ${formatError(error)}`,
      );
      setTimeout(() => {
        if (this.disposed || !this.active.has(run.claimed.assignment.id) || run.finishing) return;
        run.finishing = true;
        void this.finishRun(run, status);
      }, POLL_MS);
      return;
    }
    if (run.continuationExpected) {
      run.finishing = false;
      return;
    }
    clearInterval(run.renewTimer);
    if (run.flushTimer) {
      clearTimeout(run.flushTimer);
      run.flushTimer = null;
    }
    await this.flushEvents(run);
    const successful = status === "review" || status === "done";
    try {
      if (successful) {
        const diff = await this.local.threadDiff(run.localThreadId);
        for (const repo of diff.repos) {
          const centralRepoId = run.repoMap.get(repo.repo_id);
          if (!centralRepoId || repo.files.length === 0) continue;
          if (Buffer.byteLength(repo.patch, "utf8") > MAX_PATCH_BYTES) {
            throw new Error(
              `${repo.repo_name} returned more than 16 MiB of changes; split the work into a smaller shared assignment.`,
            );
          }
          await this.coordinator.reportCollaborationChangeSet({
            assignment_id: run.claimed.assignment.id,
            lease_token: run.claimed.lease_token,
            repo_id: centralRepoId,
            base_ref: repo.base_ref,
            files: repo.files,
            patch: repo.patch,
          });
        }
      }
      await this.coordinator.finishCollaborationAssignment({
        assignment_id: run.claimed.assignment.id,
        lease_token: run.claimed.lease_token,
        state: successful ? "completed" : status === "failed" ? "failed" : "interrupted",
        error: successful ? null : `Local worker ended with status ${status}`,
      });
    } catch (error) {
      await this.coordinator.finishCollaborationAssignment({
        assignment_id: run.claimed.assignment.id,
        lease_token: run.claimed.lease_token,
        state: "failed",
        error: formatError(error),
      }).catch(() => undefined);
      this.output.appendLine(
        `[collaboration-worker] could not publish final result: ${formatError(error)}`,
      );
    } finally {
      this.active.delete(run.claimed.assignment.id);
      this.localThreadToAssignment.delete(run.localThreadId);
      await this.local.deleteAgentThread(run.localThreadId, true).catch(() => undefined);
    }
  }

  private async syncInstructions(run: ActiveRemoteRun): Promise<void> {
    if (this.disposed) return;
    const instructions = await this.coordinator.listQueuedTurns(
      run.claimed.assignment.thread_id,
    ) as QueuedTurn[];
    for (const instruction of instructions) {
      if (!run.deliveredInstructions.has(instruction.id)) {
        await this.coordinator.reportCollaborationEvent({
          assignment_id: run.claimed.assignment.id,
          lease_token: run.claimed.lease_token,
          event_id: `instruction:${instruction.id}`,
          role: "user",
          kind: "user_message",
          text: instruction.message,
          client_message_id: instruction.client_message_id,
          data: { follow_up: true },
          ts: instruction.created_at,
        });
        await this.local.sendThreadMessage(
          run.localThreadId,
          run.claimed.assignment.agent,
          run.claimed.assignment.permission,
          instruction.message,
          instruction.client_message_id,
        );
        run.deliveredInstructions.add(instruction.id);
        run.continuationExpected = true;
      }
      await this.coordinator.deleteQueuedTurn(instruction.id);
    }
  }

  private async syncApprovalDecisions(run: ActiveRemoteRun): Promise<void> {
    if (this.disposed || run.finishing) return;
    const pending = await this.local.listPendingApprovals();
    for (const approval of pending as ApprovalRequest[]) {
      if (
        approval.thread_id === run.localThreadId &&
        !run.reportedApprovals.has(approval.id)
      ) {
        await this.reportApproval(run, approval);
      }
    }
    const decisions = await this.coordinator.listCollaborationApprovalDecisions(
      run.claimed.assignment.id,
      run.claimed.lease_token,
    );
    for (const decision of decisions) {
      await this.local.resolveApproval(
        decision.local_approval_id,
        decision.decision,
      );
      await this.coordinator.acknowledgeCollaborationApprovalDecision(
        run.claimed.assignment.id,
        run.claimed.lease_token,
        decision.id,
      );
    }
  }
}

function mapRepositories(
  bindings: AgentThreadRepo[],
  centralRepos: Repo[],
  localRepos: Repo[],
): {
  repoIds: string[];
  centralToLocal: Map<string, string>;
  localToCentral: Map<string, string>;
} {
  const centralById = new Map(centralRepos.map((repo) => [repo.id, repo]));
  const centralToLocal = new Map<string, string>();
  const localToCentral = new Map<string, string>();
  const used = new Set<string>();
  for (const binding of bindings) {
    const central = centralById.get(binding.repo_id);
    const candidates = localRepos.filter((local) => {
      if (used.has(local.id)) return false;
      const remoteMatch =
        central?.remote_url &&
        local.remote_url &&
        normalizeRemote(central.remote_url) === normalizeRemote(local.remote_url);
      return remoteMatch || local.name.toLowerCase() === binding.repo_name.toLowerCase();
    });
    const local =
      candidates[0] ??
      (bindings.length === 1 && localRepos.length === 1 && !used.has(localRepos[0].id)
        ? localRepos[0]
        : undefined);
    if (!local) continue;
    used.add(local.id);
    centralToLocal.set(binding.repo_id, local.id);
    localToCentral.set(local.id, binding.repo_id);
  }
  return { repoIds: [...localToCentral.keys()], centralToLocal, localToCentral };
}

function normalizeRemote(value: string): string {
  return value
    .trim()
    .replace(/^git@([^:]+):/, "https://$1/")
    .replace(/\.git$/i, "")
    .replace(/\/$/, "")
    .toLowerCase();
}

function isStreaming(event: AgentThreadEvent): boolean {
  return Boolean(
    event.data &&
      typeof event.data === "object" &&
      "streaming" in event.data &&
      (event.data as { streaming?: boolean }).streaming,
  );
}

function formatError(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function sanitizeApprovalForCoordinator(approval: ApprovalRequest): ApprovalRequest {
  return {
    ...approval,
    command: approval.command?.map(redactSensitiveText) ?? null,
    input: redactSensitiveValue(approval.input, 0),
    reason: approval.reason ? redactSensitiveText(approval.reason) : null,
  };
}

function redactSensitiveValue(value: unknown, depth: number): unknown {
  if (depth > 6) return "[truncated]";
  if (typeof value === "string") return redactSensitiveText(value);
  if (Array.isArray(value)) {
    return value.slice(0, 200).map((item) => redactSensitiveValue(item, depth + 1));
  }
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .slice(0, 200)
      .map(([key, child]) => [
        key,
        /token|secret|password|authorization|api[_-]?key|credential/i.test(key)
          ? "[redacted]"
          : redactSensitiveValue(child, depth + 1),
      ]),
  );
}

function redactSensitiveText(value: string): string {
  return value
    .replace(/(Bearer\s+)[A-Za-z0-9._~+\/-=]+/gi, "$1[redacted]")
    .replace(
      /((?:token|secret|password|authorization|api[_-]?key|credential)\s*[=:]\s*)([^\s,;]+)/gi,
      "$1[redacted]",
    );
}

export const collaborationWorkerTestExports = {
  mapRepositories,
  normalizeRemote,
  sanitizeApprovalForCoordinator,
};
