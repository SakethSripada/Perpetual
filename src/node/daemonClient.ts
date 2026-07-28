import { EventEmitter } from "node:events";
import net from "node:net";
import type { Duplex } from "node:stream";
import {
  expectUnit,
  responsePayload,
  variant,
  type DaemonApi,
  type DaemonRequest,
  type DaemonResponse,
  type ServerMessage,
} from "./protocol";

type Pending = {
  resolve: (value: DaemonResponse) => void;
  reject: (reason: Error) => void;
};

export class DaemonClient extends EventEmitter implements DaemonApi {
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private buffer = "";

  private constructor(private readonly socket: Duplex) {
    super();
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => this.onData(chunk.toString()));
    socket.on("error", (err) => this.failAll(err));
    socket.on("close", () => this.failAll(new Error("daemon connection closed")));
  }

  static connect(port: number, token: string): Promise<DaemonClient> {
    const socket = net.createConnection({ host: "127.0.0.1", port });
    return this.connectStream(socket, token, true);
  }

  static connectStream(
    socket: Duplex,
    token: string,
    waitForConnect = false
  ): Promise<DaemonClient> {
    return new Promise((resolve, reject) => {
      socket.setEncoding("utf8");
      let buffer = "";
      const fail = (err: Error) => {
        socket.destroy();
        reject(err);
      };
      socket.once("error", fail);
      const sendHandshake = () => {
        socket.write(`${JSON.stringify({ token, capabilities: ["events_v2"] })}\n`);
      };
      if (waitForConnect) socket.once("connect", sendHandshake);
      else sendHandshake();
      socket.on("data", function onHandshake(chunk) {
        buffer += chunk.toString();
        const idx = buffer.indexOf("\n");
        if (idx < 0) return;
        socket.off("data", onHandshake);
        socket.off("error", fail);
        const line = buffer.slice(0, idx).trim();
        const rest = buffer.slice(idx + 1);
        try {
          const ack = JSON.parse(line) as { ok: boolean; error?: string };
          if (!ack.ok) {
            fail(new Error(ack.error ?? "daemon rejected authentication"));
            return;
          }
          const client = new DaemonClient(socket);
          if (rest) client.onData(rest);
          resolve(client);
        } catch (err) {
          fail(err instanceof Error ? err : new Error(String(err)));
        }
      });
    });
  }

  dispose(): void {
    this.socket.destroy();
    this.failAll(new Error("daemon client disposed"));
  }

  async requestRaw(request: DaemonRequest): Promise<DaemonResponse> {
    const id = this.nextId++;
    const frame = JSON.stringify({ id, request });
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.socket.write(`${frame}\n`, (err) => {
        if (!err) return;
        this.pending.delete(id);
        reject(err);
      });
    });
  }

  async ping(): Promise<void> {
    const response = await this.requestRaw(variant("ping"));
    if (response !== "pong") throw new Error("daemon did not pong");
  }

  async ensureWorkbenchProject() {
    return responsePayload(await this.requestRaw(variant("ensure_workbench_project")), "project");
  }

  async connectLocalRepo(input: Parameters<DaemonApi["connectLocalRepo"]>[0]) {
    return responsePayload(await this.requestRaw(variant("connect_local_repo", input)), "repo");
  }

  async listRepos(projectId: string) {
    return responsePayload(await this.requestRaw(variant("list_repos", { project_id: projectId })), "repos");
  }

  async deleteRepo(repoId: string) {
    expectUnit(await this.requestRaw(variant("delete_repo", { repo_id: repoId })));
  }

  async clearProjectRepos(projectId: string) {
    expectUnit(await this.requestRaw(variant("clear_project_repos", { project_id: projectId })));
  }

  async githubAuthStatus(token: string) {
    return responsePayload(await this.requestRaw(variant("github_auth_status", { token })), "github_auth_status");
  }

  async githubListRepositories(token: string) {
    return responsePayload(
      await this.requestRaw(variant("github_list_repositories", { token })),
      "github_repositories"
    );
  }

  async connectGithubRepo(token: string, input: Parameters<DaemonApi["connectGithubRepo"]>[1]) {
    return responsePayload(await this.requestRaw(variant("connect_github_repo", { token, input })), "repo");
  }

  async detectAgents() {
    return responsePayload(await this.requestRaw(variant("detect_agents")), "agent_statuses");
  }

  async agentRunDefaults() {
    return responsePayload(await this.requestRaw(variant("agent_run_defaults")), "agent_run_defaults");
  }

  async agentModelCatalog() {
    return responsePayload(await this.requestRaw(variant("agent_model_catalog")), "agent_model_catalogs");
  }

  async detectLocalModels() {
    return responsePayload(await this.requestRaw(variant("detect_local_models")), "local_model_statuses");
  }

  async getLocalModelPolicy() {
    return responsePayload(await this.requestRaw(variant("get_local_model_policy")), "local_model_policy");
  }

  async setLocalModelPolicy(policy: Parameters<DaemonApi["setLocalModelPolicy"]>[0]) {
    return responsePayload(await this.requestRaw(variant("set_local_model_policy", policy)), "local_model_policy");
  }

  async getLimitPolicy() {
    return responsePayload(await this.requestRaw(variant("get_limit_policy")), "limit_policy");
  }

  async setLimitPolicy(policy: Parameters<DaemonApi["setLimitPolicy"]>[0]) {
    return responsePayload(await this.requestRaw(variant("set_limit_policy", policy)), "limit_policy");
  }

  async detectSandboxRuntime() {
    return responsePayload(await this.requestRaw(variant("detect_sandbox_runtime")), "sandbox_runtime_status");
  }

  async sandboxLogin() {
    return responsePayload(await this.requestRaw(variant("sandbox_login")), "sandbox_login_prompt");
  }

  async codexSandboxLogin() {
    return responsePayload(await this.requestRaw(variant("codex_sandbox_login")), "sandbox_login_prompt");
  }

  async getSandboxPolicy() {
    return responsePayload(await this.requestRaw(variant("get_sandbox_policy")), "sandbox_policy");
  }

  async setSandboxPolicy(policy: Parameters<DaemonApi["setSandboxPolicy"]>[0]) {
    return responsePayload(await this.requestRaw(variant("set_sandbox_policy", policy)), "sandbox_policy");
  }

  async getCloudPolicy() {
    return responsePayload(await this.requestRaw(variant("get_cloud_policy")), "cloud_policy");
  }

  async setCloudPolicy(policy: Parameters<DaemonApi["setCloudPolicy"]>[0]) {
    return responsePayload(await this.requestRaw(variant("set_cloud_policy", policy)), "cloud_policy");
  }

  async cloudAvailability() {
    return responsePayload(await this.requestRaw(variant("cloud_availability")), "cloud_availabilities");
  }

  async listCloudRuns(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("list_cloud_runs", { thread_id: threadId })),
      "cloud_runs"
    );
  }

  async launchCloudHandoff(threadId: string, agent?: Parameters<DaemonApi["launchCloudHandoff"]>[1]) {
    return responsePayload(
      await this.requestRaw(
        variant("launch_cloud_handoff", { thread_id: threadId, agent: agent ?? null })
      ),
      "cloud_run"
    );
  }

  async reclaimCloudRun(threadId: string) {
    expectUnit(await this.requestRaw(variant("reclaim_cloud_run", { thread_id: threadId })));
  }

  async listActivity(projectId?: string | null, limit?: number | null) {
    return responsePayload(
      await this.requestRaw(
        variant("list_activity", { project_id: projectId ?? null, limit: limit ?? null })
      ),
      "activity"
    );
  }

  async getWorkGraph(projectId: string) {
    return responsePayload(
      await this.requestRaw(variant("get_work_graph", { project_id: projectId })),
      "work_graph"
    );
  }

  async createWorkNode(input: Parameters<DaemonApi["createWorkNode"]>[0]) {
    return responsePayload(await this.requestRaw(variant("create_work_node", input)), "work_node");
  }

  async updateWorkNode(nodeId: string, patch: Parameters<DaemonApi["updateWorkNode"]>[1]) {
    return responsePayload(
      await this.requestRaw(variant("update_work_node", { node_id: nodeId, patch })),
      "work_node"
    );
  }

  async deleteWorkNode(nodeId: string) {
    expectUnit(await this.requestRaw(variant("delete_work_node", { node_id: nodeId })));
  }

  async moveWorkNode(
    nodeId: string,
    parentId: string | null,
    positionX: number,
    positionY: number
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("move_work_node", {
          node_id: nodeId,
          parent_id: parentId,
          position_x: positionX,
          position_y: positionY,
        })
      ),
      "work_node"
    );
  }

  async connectWorkNodes(input: Parameters<DaemonApi["connectWorkNodes"]>[0]) {
    return responsePayload(await this.requestRaw(variant("connect_work_nodes", input)), "work_edge");
  }

  async assignWorkNodeRepos(nodeId: string, repoIds: string[]) {
    return responsePayload(
      await this.requestRaw(variant("assign_work_node_repos", { node_id: nodeId, repo_ids: repoIds })),
      "work_node_repo_bindings"
    );
  }

  async runWorkNode(
    nodeId: string,
    agent: Parameters<DaemonApi["runWorkNode"]>[1],
    permission: Parameters<DaemonApi["runWorkNode"]>[2],
    executionBackend: Parameters<DaemonApi["runWorkNode"]>[3]
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("run_work_node", {
          node_id: nodeId,
          agent,
          permission,
          execution_backend: executionBackend,
        })
      ),
      "session_id"
    );
  }

  async stopWorkNode(nodeId: string) {
    expectUnit(await this.requestRaw(variant("stop_work_node", { node_id: nodeId })));
  }

  async sendWorkNodeMessage(
    nodeId: string,
    agent: Parameters<DaemonApi["sendWorkNodeMessage"]>[1],
    permission: Parameters<DaemonApi["sendWorkNodeMessage"]>[2],
    message: string
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("send_work_node_message", { node_id: nodeId, agent, permission, message })
      ),
      "turn_id_opt"
    );
  }

  async previewContextPacket(nodeId: string) {
    return responsePayload(
      await this.requestRaw(variant("preview_context_packet", { node_id: nodeId })),
      "context_packet"
    );
  }

  async workNodeDiff(nodeId: string) {
    return responsePayload(
      await this.requestRaw(variant("work_node_diff", { node_id: nodeId })),
      "work_node_diff"
    );
  }

  async listAgentThreads(projectId?: string) {
    return responsePayload(
      await this.requestRaw(variant("list_agent_threads", { project_id: projectId ?? null })),
      "agent_threads"
    );
  }

  async getAgentThread(id: string) {
    return responsePayload(
      await this.requestRaw(variant("get_agent_thread", { id })),
      "agent_thread_opt"
    );
  }

  async createAgentThread(input: Parameters<DaemonApi["createAgentThread"]>[0]) {
    return responsePayload(await this.requestRaw(variant("create_agent_thread", input)), "agent_thread");
  }

  async updateAgentThread(id: string, patch: Parameters<DaemonApi["updateAgentThread"]>[1]) {
    return responsePayload(await this.requestRaw(variant("update_agent_thread", { id, patch })), "agent_thread");
  }

  async deleteAgentThread(id: string, force: boolean) {
    expectUnit(await this.requestRaw(variant("delete_agent_thread", { id, force })));
  }

  async assignThreadRepos(threadId: string, repoIds: string[]) {
    return responsePayload(
      await this.requestRaw(variant("assign_thread_repos", { thread_id: threadId, repo_ids: repoIds })),
      "agent_thread_repos"
    );
  }

  async listThreadRepos(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("list_thread_repos", { thread_id: threadId })),
      "agent_thread_repos"
    );
  }

  async threadDiff(threadId: string) {
    return responsePayload(await this.requestRaw(variant("thread_diff", { thread_id: threadId })), "agent_thread_diff");
  }

  async applyThreadChanges(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("apply_thread_changes", { thread_id: threadId })),
      "agent_thread_apply_result"
    );
  }

  async runAgentThread(
    threadId: string,
    agent: Parameters<DaemonApi["runAgentThread"]>[1],
    permission: Parameters<DaemonApi["runAgentThread"]>[2],
    message: string | null,
    executionBackend: Parameters<DaemonApi["runAgentThread"]>[4],
    clientMessageId?: string | null
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("run_agent_thread", {
          thread_id: threadId,
          agent,
          permission,
          message,
          execution_backend: executionBackend,
          client_message_id: clientMessageId ?? null,
        })
      ),
      "turn_id"
    );
  }

  async sendThreadMessage(
    threadId: string,
    agent: Parameters<DaemonApi["sendThreadMessage"]>[1],
    permission: Parameters<DaemonApi["sendThreadMessage"]>[2],
    message: string,
    clientMessageId?: string | null
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("send_thread_message", {
          thread_id: threadId,
          agent,
          permission,
          message,
          client_message_id: clientMessageId ?? null,
        })
      ),
      "turn_id_opt"
    );
  }

  async stopAgentThread(threadId: string) {
    expectUnit(await this.requestRaw(variant("stop_agent_thread", { thread_id: threadId })));
  }

  async listPendingApprovals() {
    return responsePayload(await this.requestRaw(variant("list_pending_approvals")), "pending_approvals");
  }

  async resolveApproval(id: string, decision: Parameters<DaemonApi["resolveApproval"]>[1]) {
    expectUnit(await this.requestRaw(variant("resolve_approval", { id, decision })));
  }

  async listThreadEvents(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("list_thread_events", { thread_id: threadId })),
      "agent_thread_events"
    );
  }

  async listThreadTurns(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("list_thread_turns", { thread_id: threadId })),
      "agent_turns"
    );
  }

  async listQueuedTurns(threadId: string) {
    return responsePayload(
      await this.requestRaw(variant("list_queued_turns", { thread_id: threadId })),
      "queued_turns"
    );
  }

  async deleteQueuedTurn(id: string) {
    expectUnit(await this.requestRaw(variant("delete_queued_turn", { id })));
  }

  async updateQueuedTurn(id: string, message: string) {
    expectUnit(await this.requestRaw(variant("update_queued_turn", { id, message })));
  }

  async reorderQueuedTurns(threadId: string, orderedIds: string[]) {
    expectUnit(
      await this.requestRaw(
        variant("reorder_queued_turns", { thread_id: threadId, ordered_ids: orderedIds })
      )
    );
  }

  async registerCollaborationDevice(input: Parameters<DaemonApi["registerCollaborationDevice"]>[0]) {
    return responsePayload(
      await this.requestRaw(variant("register_collaboration_device", input)),
      "collaboration_device"
    );
  }

  async heartbeatCollaborationDevice(input: Parameters<DaemonApi["heartbeatCollaborationDevice"]>[0]) {
    return responsePayload(
      await this.requestRaw(variant("heartbeat_collaboration_device", input)),
      "collaboration_device"
    );
  }

  async listCollaborationDevices() {
    return responsePayload(
      await this.requestRaw(variant("list_collaboration_devices")),
      "collaboration_devices"
    );
  }

  async revokeCollaborationDevice(deviceId: string) {
    expectUnit(
      await this.requestRaw(variant("revoke_collaboration_device", { device_id: deviceId }))
    );
  }

  async collaborationSnapshot(threadId?: string | null, includePatches = false) {
    return responsePayload(
      await this.requestRaw(
        variant("collaboration_snapshot", {
          thread_id: threadId ?? null,
          include_patches: includePatches,
        })
      ),
      "collaboration_snapshot"
    );
  }

  async createCollaborationAssignment(
    input: Parameters<DaemonApi["createCollaborationAssignment"]>[0]
  ) {
    return responsePayload(
      await this.requestRaw(variant("create_collaboration_assignment", input)),
      "collaboration_assignment"
    );
  }

  async listCollaborationAssignments(deviceId?: string | null, activeOnly = false) {
    return responsePayload(
      await this.requestRaw(
        variant("list_collaboration_assignments", {
          device_id: deviceId ?? null,
          active_only: activeOnly,
        })
      ),
      "collaboration_assignments"
    );
  }

  async claimCollaborationAssignment(assignmentId: string, deviceId: string) {
    return responsePayload(
      await this.requestRaw(
        variant("claim_collaboration_assignment", {
          assignment_id: assignmentId,
          device_id: deviceId,
        })
      ),
      "claimed_collaboration_assignment"
    );
  }

  async renewCollaborationLease(assignmentId: string, leaseToken: string) {
    return responsePayload(
      await this.requestRaw(
        variant("renew_collaboration_lease", {
          assignment_id: assignmentId,
          lease_token: leaseToken,
        })
      ),
      "collaboration_assignment"
    );
  }

  async reportCollaborationEvent(input: Parameters<DaemonApi["reportCollaborationEvent"]>[0]) {
    expectUnit(await this.requestRaw(variant("report_collaboration_event", input)));
  }

  async reportCollaborationApproval(
    input: Parameters<DaemonApi["reportCollaborationApproval"]>[0]
  ) {
    expectUnit(await this.requestRaw(variant("report_collaboration_approval", input)));
  }

  async listCollaborationApprovalDecisions(
    assignmentId: string,
    leaseToken: string
  ) {
    return responsePayload(
      await this.requestRaw(
        variant("list_collaboration_approval_decisions", {
          assignment_id: assignmentId,
          lease_token: leaseToken,
        })
      ),
      "collaboration_approval_decisions"
    );
  }

  async acknowledgeCollaborationApprovalDecision(
    assignmentId: string,
    leaseToken: string,
    approvalId: string
  ) {
    expectUnit(
      await this.requestRaw(
        variant("acknowledge_collaboration_approval_decision", {
          assignment_id: assignmentId,
          lease_token: leaseToken,
          approval_id: approvalId,
        })
      )
    );
  }

  async reportCollaborationChangeSet(
    input: Parameters<DaemonApi["reportCollaborationChangeSet"]>[0]
  ) {
    return responsePayload(
      await this.requestRaw(variant("report_collaboration_change_set", input)),
      "collaboration_change_set"
    );
  }

  async finishCollaborationAssignment(
    input: Parameters<DaemonApi["finishCollaborationAssignment"]>[0]
  ) {
    return responsePayload(
      await this.requestRaw(variant("finish_collaboration_assignment", input)),
      "collaboration_assignment"
    );
  }

  async cancelCollaborationAssignment(assignmentId: string) {
    return responsePayload(
      await this.requestRaw(
        variant("cancel_collaboration_assignment", { assignment_id: assignmentId })
      ),
      "collaboration_assignment"
    );
  }

  async applyCollaborationChangeSet(changeSetId: string, overwrite = false) {
    return responsePayload(
      await this.requestRaw(
        variant("apply_collaboration_change_set", {
          change_set_id: changeSetId,
          overwrite,
        })
      ),
      "collaboration_change_set"
    );
  }

  async rejectCollaborationChangeSet(changeSetId: string) {
    return responsePayload(
      await this.requestRaw(
        variant("reject_collaboration_change_set", { change_set_id: changeSetId })
      ),
      "collaboration_change_set"
    );
  }

  async importCollaborationPatch(threadId: string, repoId: string, patch: string) {
    expectUnit(
      await this.requestRaw(
        variant("import_collaboration_patch", {
          thread_id: threadId,
          repo_id: repoId,
          patch,
        })
      )
    );
  }

  async prepareShutdown() {
    expectUnit(await this.requestRaw(variant("prepare_shutdown")));
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    for (;;) {
      const idx = this.buffer.indexOf("\n");
      if (idx < 0) break;
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      this.onLine(line);
    }
  }

  private onLine(line: string): void {
    let message: ServerMessage;
    try {
      message = JSON.parse(line) as ServerMessage;
    } catch (err) {
      this.emit("error", err);
      return;
    }

    if ("event" in message) {
      this.emit("event", message.event);
      return;
    }

    if ("event_v2" in message) {
      this.emit("event", message.event_v2.event);
      return;
    }

    if ("event_gap" in message) {
      this.emit("event_gap", message.event_gap);
      return;
    }

    const { id, ok, err } = message.response;
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    if (err) {
      pending.reject(new Error(err));
    } else if (ok !== undefined) {
      pending.resolve(ok);
    } else {
      pending.reject(new Error("empty daemon response"));
    }
  }

  private failAll(err: Error): void {
    for (const pending of this.pending.values()) {
      pending.reject(err);
    }
    this.pending.clear();
    this.emit("disconnect", err);
  }
}
