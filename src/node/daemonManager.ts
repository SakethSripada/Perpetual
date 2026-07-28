import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import * as vscode from "vscode";
import {
  CollaborationHost,
  connectCollaborationPeer,
  decodeCollaborationInvite,
  type SavedCollaborationPeer,
} from "./collaborationTransport";
import { DaemonClient } from "./daemonClient";
import type { AgentStatus, AppEvent, RegisterCollaborationDevice } from "./types";

type Endpoint = {
  port: number;
  token: string;
};

export class DaemonManager implements vscode.Disposable {
  private client: DaemonClient | null = null;
  private coordinatorClient: DaemonClient | null = null;
  private collaborationHost: CollaborationHost | null = null;
  private child: ChildProcessWithoutNullStreams | null = null;
  private startPromise: Promise<DaemonClient> | null = null;
  private restorePromise: Promise<void> | null = null;
  private collaborationRestored = false;
  private heartbeatTimer: NodeJS.Timeout | null = null;
  private disposed = false;
  private stdoutBuffer = "";
  private stderrBuffer = "";
  private readonly events = new vscode.EventEmitter<AppEvent>();

  readonly onEvent = this.events.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly output: vscode.OutputChannel
  ) {}

  async getClient(): Promise<DaemonClient> {
    const local = await this.getLocalClient();
    await this.restoreCollaboration(local);
    return this.coordinatorClient ?? local;
  }

  async getLocalClient(): Promise<DaemonClient> {
    if (this.disposed) throw new Error("Perpetual daemon manager is disposed");
    if (this.client) return this.client;
    if (this.startPromise) return this.startPromise;

    this.startPromise = this.startDaemon().finally(() => {
      this.startPromise = null;
    });
    return this.startPromise;
  }

  async createCollaborationInvite(): Promise<string> {
    const local = await this.getLocalClient();
    const identity = await this.deviceIdentity();
    if (!this.collaborationHost) {
      const hostId = this.context.globalState.get<string>(HOST_ID_KEY) ?? randomUUID();
      await this.context.globalState.update(HOST_ID_KEY, hostId);
      const host = new CollaborationHost(
        local,
        this.context.secrets,
        hostId,
        identity.name,
      );
      host.on("error", (error) =>
        this.output.appendLine(`[collaboration] host error: ${formatError(error)}`),
      );
      const preferredPort = this.context.globalState.get<number>(HOST_PORT_KEY, 0);
      let port: number;
      try {
        port = await host.start(preferredPort);
      } catch (error) {
        if (!preferredPort) throw error;
        this.output.appendLine(
          `[collaboration] saved port ${preferredPort} unavailable; selecting a new port`,
        );
        port = await host.start(0);
      }
      this.collaborationHost = host;
      await Promise.all([
        this.context.globalState.update(HOST_ENABLED_KEY, true),
        this.context.globalState.update(HOST_PORT_KEY, port),
      ]);
      await this.registerThisDevice(local);
      this.startHeartbeat();
      this.output.appendLine(`[collaboration] hosting encrypted workspace on port ${port}`);
    }
    return this.collaborationHost.createInvite().text;
  }

  async joinCollaboration(inviteText: string): Promise<void> {
    const invite = decodeCollaborationInvite(inviteText);
    const local = await this.getLocalClient();
    const identity = await this.deviceIdentity();
    const connected = await connectCollaborationPeer(invite, identity.id);
    const coordinator = await DaemonClient.connectStream(connected.stream, connected.credential);
    try {
      await this.registerThisDevice(coordinator);
    } catch (error) {
      coordinator.dispose();
      throw error;
    }
    const saved: SavedCollaborationPeer = {
      v: 1,
      hostId: invite.hostId,
      hostName: invite.hostName,
      addresses: invite.addresses,
      port: invite.port,
      inviteId: invite.inviteId,
      deviceId: identity.id,
      credential: connected.credential,
    };
    await this.context.secrets.store(PEER_SECRET_KEY, JSON.stringify(saved));
    this.useCoordinatorClient(coordinator);
    this.collaborationRestored = true;
    this.startHeartbeat();
    this.output.appendLine(`[collaboration] joined ${invite.hostName}`);
  }

  async leaveCollaboration(): Promise<void> {
    this.coordinatorClient?.dispose();
    this.coordinatorClient = null;
    await this.context.secrets.delete(PEER_SECRET_KEY);
    this.collaborationRestored = true;
    if (!this.collaborationHost && this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    this.output.appendLine("[collaboration] left shared workspace");
  }

  async stopCollaborationHost(): Promise<void> {
    this.collaborationHost?.dispose();
    this.collaborationHost = null;
    await this.context.globalState.update(HOST_ENABLED_KEY, false);
    if (!this.coordinatorClient && this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  async revokeCollaborationDevice(deviceId: string): Promise<void> {
    await this.collaborationHost?.revokeDevice(deviceId);
    const client = await this.getClient();
    await client.revokeCollaborationDevice(deviceId);
  }

  async collaborationStatus(): Promise<{
    role: "standalone" | "host" | "member";
    connected: boolean;
    hostName: string | null;
    deviceId: string;
    deviceName: string;
  }> {
    const identity = await this.deviceIdentity();
    const saved = await this.savedPeer();
    return {
      role: this.coordinatorClient ? "member" : this.collaborationHost ? "host" : "standalone",
      connected: Boolean(this.coordinatorClient || this.collaborationHost),
      hostName: this.coordinatorClient ? saved?.hostName ?? null : this.collaborationHost ? identity.name : null,
      deviceId: identity.id,
      deviceName: identity.name,
    };
  }

  async ping(): Promise<void> {
    const client = await this.getClient();
    await client.ping();
  }

  async prepareShutdown(timeoutMs = 360_000): Promise<void> {
    const client = this.client;
    if (client) {
      try {
        await withTimeout(client.prepareShutdown(), timeoutMs);
        this.output.appendLine("[daemon] shutdown prepared");
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        this.output.appendLine(`[daemon] shutdown prepare skipped: ${message}`);
      }
    }
    this.coordinatorClient?.dispose();
    this.coordinatorClient = null;
    this.collaborationHost?.dispose();
    this.collaborationHost = null;
    this.dispose();
  }

  dispose(): void {
    this.disposed = true;
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = null;
    this.coordinatorClient?.dispose();
    this.coordinatorClient = null;
    this.collaborationHost?.dispose();
    this.collaborationHost = null;
    this.client?.dispose();
    this.client = null;
    if (this.child && !this.child.killed) {
      this.child.kill();
    }
    this.child = null;
    this.events.dispose();
  }

  private async startDaemon(): Promise<DaemonClient> {
    const binary = this.resolveBinary();
    const dataDir = path.join(this.context.globalStorageUri.fsPath, "daemon-data");
    const endpointPath = path.join(dataDir, "daemon.json");

    fs.mkdirSync(dataDir, { recursive: true });
    fs.rmSync(endpointPath, { force: true });

    this.output.appendLine(`[daemon] starting ${binary}`);
    const child = spawn(binary, [], {
      cwd: this.context.extensionPath,
      env: {
        ...process.env,
        PERPETUAL_DATA_DIR: dataDir,
        PERPETUAL_DAEMON_PORT: "0",
      },
      stdio: "pipe",
      windowsHide: true,
    });
    this.child = child;

    child.stdout.on("data", (chunk) => this.appendDaemonOutput("out", chunk.toString()));
    child.stderr.on("data", (chunk) => this.appendDaemonOutput("err", chunk.toString()));
    child.once("exit", (code, signal) => {
      this.output.appendLine(`[daemon] exited code=${code ?? "null"} signal=${signal ?? "null"}`);
      this.client?.dispose();
      this.client = null;
      if (this.child === child) this.child = null;
    });

    try {
      const endpoint = await waitForEndpoint(endpointPath, () => {
        if (child.exitCode !== null) {
          throw new Error(`am-daemon exited before writing endpoint file (code ${child.exitCode})`);
        }
      });
      const client = await DaemonClient.connect(endpoint.port, endpoint.token);
      client.on("event", (event: AppEvent) => {
        if (!this.coordinatorClient) this.events.fire(event);
      });
      client.on("event_gap", (gap: unknown) =>
        this.events.fire({ type: "event_gap", data: gap } as AppEvent),
      );
      client.on("disconnect", (err: Error) => {
        if (!this.disposed) this.output.appendLine(`[daemon] disconnected: ${err.message}`);
        if (this.client === client) this.client = null;
        // A broken socket is not a healthy daemon session. Terminate the
        // associated child so a reconnect cannot leave an orphaned daemon
        // holding the database and agent processes open.
        if (!this.disposed && this.child === child && child.exitCode === null) {
          child.kill();
        }
      });
      this.client = client;
      return client;
    } catch (err) {
      if (this.child === child) this.child = null;
      if (child.exitCode === null) child.kill();
      throw err;
    }
  }

  private appendDaemonOutput(kind: "out" | "err", chunk: string): void {
    if (kind === "out") {
      this.stdoutBuffer += chunk;
    } else {
      this.stderrBuffer += chunk;
    }
    let emitted = 0;
    for (;;) {
      const current = kind === "out" ? this.stdoutBuffer : this.stderrBuffer;
      const idx = current.indexOf("\n");
      if (idx < 0) break;
      const line = current.slice(0, idx).trimEnd();
      if (kind === "out") {
        this.stdoutBuffer = current.slice(idx + 1);
      } else {
        this.stderrBuffer = current.slice(idx + 1);
      }
      if (!line.trim()) continue;
      emitted += 1;
      if (emitted <= 20) {
        this.output.appendLine(`[daemon:${kind}] ${summarizeDaemonLine(line)}`);
      }
    }
    if (emitted > 20) {
      this.output.appendLine(`[daemon:${kind}] ${emitted - 20} additional log lines suppressed`);
    }
    const tail = kind === "out" ? this.stdoutBuffer : this.stderrBuffer;
    if (tail.length > 4096) {
      this.output.appendLine(`[daemon:${kind}] ${summarizeDaemonLine(tail)}`);
      if (kind === "out") {
        this.stdoutBuffer = "";
      } else {
        this.stderrBuffer = "";
      }
    }
  }

  private resolveBinary(): string {
    const configured = vscode.workspace
      .getConfiguration("perpetual")
      .get<string>("daemonPath", "")
      .trim();
    if (configured) {
      if (!fs.existsSync(configured)) {
        throw new Error(`Configured Perpetual daemon does not exist: ${configured}`);
      }
      return configured;
    }

    const target = currentTarget();
    const bundled = path.join(this.context.extensionPath, "bin", target, binaryName());
    if (fs.existsSync(bundled)) return bundled;

    for (const devBinary of devBinaryCandidates(this.context.extensionPath, target)) {
      if (fs.existsSync(devBinary)) return devBinary;
    }

    throw new Error(
      `No bundled am-daemon binary found for ${target}. Run npm run build:daemon -- --target=${target} && npm run copy-daemon -- --target=${target}, or set perpetual.daemonPath.`
    );
  }

  private async restoreCollaboration(local: DaemonClient): Promise<void> {
    if (this.collaborationRestored) return;
    if (this.restorePromise) return this.restorePromise;
    this.restorePromise = this.restoreCollaborationNow(local).finally(() => {
      this.restorePromise = null;
      this.collaborationRestored = true;
    });
    return this.restorePromise;
  }

  private async restoreCollaborationNow(local: DaemonClient): Promise<void> {
    if (this.context.globalState.get<boolean>(HOST_ENABLED_KEY, false)) {
      try {
        await this.createCollaborationInvite();
      } catch (error) {
        this.output.appendLine(
          `[collaboration] could not restore host: ${formatError(error)}`,
        );
      }
    }
    const saved = await this.savedPeer();
    if (!saved) return;
    try {
      const connected = await connectCollaborationPeer(saved, saved.deviceId);
      const coordinator = await DaemonClient.connectStream(
        connected.stream,
        connected.credential,
      );
      if (connected.credential !== saved.credential) {
        await this.context.secrets.store(
          PEER_SECRET_KEY,
          JSON.stringify({ ...saved, credential: connected.credential }),
        );
      }
      await this.registerThisDevice(coordinator);
      this.useCoordinatorClient(coordinator);
      this.startHeartbeat();
      this.output.appendLine(`[collaboration] reconnected to ${saved.hostName}`);
    } catch (error) {
      this.output.appendLine(
        `[collaboration] ${saved.hostName} is offline: ${formatError(error)}`,
      );
    }
    void local;
  }

  private useCoordinatorClient(client: DaemonClient): void {
    this.coordinatorClient?.dispose();
    this.coordinatorClient = client;
    client.on("event", (event: AppEvent) => this.events.fire(event));
    client.on("event_gap", (gap: unknown) =>
      this.events.fire({ type: "event_gap", data: gap } as AppEvent),
    );
    client.on("disconnect", (error: Error) => {
      if (this.coordinatorClient !== client || this.disposed) return;
      this.output.appendLine(`[collaboration] coordinator disconnected: ${error.message}`);
      this.coordinatorClient = null;
    });
  }

  private async registerThisDevice(client: DaemonClient): Promise<void> {
    const input = await this.deviceRegistration();
    await client.registerCollaborationDevice(input);
  }

  private startHeartbeat(): void {
    if (this.heartbeatTimer) return;
    this.heartbeatTimer = setInterval(() => void this.heartbeat(), 15_000);
  }

  private async heartbeat(): Promise<void> {
    try {
      const input = await this.deviceRegistration();
      if (this.collaborationHost && this.client) {
        await this.client.heartbeatCollaborationDevice(input);
      }
      if (this.coordinatorClient) {
        await this.coordinatorClient.heartbeatCollaborationDevice(input);
      } else if (await this.savedPeer()) {
        this.collaborationRestored = false;
        if (this.client) await this.restoreCollaboration(this.client);
      }
    } catch (error) {
      this.output.appendLine(`[collaboration] heartbeat delayed: ${formatError(error)}`);
    }
  }

  private async deviceRegistration(): Promise<RegisterCollaborationDevice> {
    const identity = await this.deviceIdentity();
    const local = await this.getLocalClient();
    const agents: AgentStatus[] = await local.detectAgents().catch(() => []);
    return {
      id: identity.id,
      name: identity.name,
      hostname: os.hostname(),
      platform: currentTarget(),
      extension_version: String(this.context.extension.packageJSON.version ?? "unknown"),
      capabilities: agents.map((agent) => ({
        agent: agent.kind,
        installed: agent.installed,
        authenticated: agent.authenticated,
        version: agent.version,
      })),
    };
  }

  private async deviceIdentity(): Promise<{ id: string; name: string }> {
    let id = this.context.globalState.get<string>(DEVICE_ID_KEY);
    if (!id) {
      id = randomUUID();
      await this.context.globalState.update(DEVICE_ID_KEY, id);
    }
    const configured = vscode.workspace
      .getConfiguration("perpetual")
      .get<string>("collaboration.deviceName", "")
      .trim();
    return { id, name: configured || friendlyDeviceName() };
  }

  private async savedPeer(): Promise<SavedCollaborationPeer | null> {
    const raw = await this.context.secrets.get(PEER_SECRET_KEY);
    if (!raw) return null;
    try {
      const peer = JSON.parse(raw) as SavedCollaborationPeer;
      return peer.v === 1 && peer.deviceId && peer.credential ? peer : null;
    } catch {
      await this.context.secrets.delete(PEER_SECRET_KEY);
      return null;
    }
  }
}

const DEVICE_ID_KEY = "perpetual.collaboration.deviceId";
const HOST_ID_KEY = "perpetual.collaboration.hostId";
const HOST_PORT_KEY = "perpetual.collaboration.hostPort";
const HOST_ENABLED_KEY = "perpetual.collaboration.hostEnabled";
const PEER_SECRET_KEY = "perpetual.collaboration.peer.v1";

function friendlyDeviceName(): string {
  const hostname = os.hostname().replace(/\.local$/i, "").trim();
  return hostname || `${os.type()} device`;
}

function formatError(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out")), timeoutMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      }
    );
  });
}

function summarizeDaemonLine(line: string): string {
  const redacted = line
    .replace(/(token|authorization|api[_-]?key|password)=([^,\s]+)/gi, "$1=[redacted]")
    .replace(/(Bearer\s+)[A-Za-z0-9._~+/=-]+/g, "$1[redacted]");
  if (redacted.length <= 500) return redacted;
  return `${redacted.slice(0, 500)} ... [truncated]`;
}

export function currentTarget(platform = process.platform, arch = process.arch): string {
  const archPart =
    arch === "x64" || arch === "arm64"
      ? arch
      : arch === "ia32"
        ? "x86"
        : arch;
  return `${platform}-${archPart}`;
}

function binaryName(): string {
  return process.platform === "win32" ? "am-daemon.exe" : "am-daemon";
}

function devBinaryCandidates(extensionPath: string, target: string): string[] {
  const binary = binaryName();
  const triple = targetTriple(target);
  return [
    path.resolve(extensionPath, "target", triple, "release", binary),
    path.resolve(extensionPath, "target", "release", binary),
  ];
}

function targetTriple(target: string): string {
  switch (target) {
    case "darwin-arm64":
      return "aarch64-apple-darwin";
    case "darwin-x64":
      return "x86_64-apple-darwin";
    case "linux-x64":
      return "x86_64-unknown-linux-gnu";
    case "linux-arm64":
      return "aarch64-unknown-linux-gnu";
    case "win32-x64":
      return "x86_64-pc-windows-msvc";
    case "win32-arm64":
      return "aarch64-pc-windows-msvc";
    default:
      return target;
  }
}

async function waitForEndpoint(
  endpointPath: string,
  checkChild: () => void,
  timeoutMs = 15_000
): Promise<Endpoint> {
  const started = Date.now();
  let lastErr: unknown = null;

  while (Date.now() - started < timeoutMs) {
    checkChild();
    try {
      const raw = fs.readFileSync(endpointPath, "utf8");
      const parsed = JSON.parse(raw) as Endpoint;
      if (typeof parsed.port === "number" && typeof parsed.token === "string") {
        return parsed;
      }
    } catch (err) {
      lastErr = err;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  const reason = lastErr instanceof Error ? ` Last read error: ${lastErr.message}` : "";
  throw new Error(`Timed out waiting for am-daemon endpoint at ${endpointPath}.${reason}`);
}
