import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import * as vscode from "vscode";
import { DaemonClient } from "./daemonClient";
import type { AppEvent } from "./types";

type Endpoint = {
  port: number;
  token: string;
};

export class DaemonManager implements vscode.Disposable {
  private client: DaemonClient | null = null;
  private child: ChildProcessWithoutNullStreams | null = null;
  private startPromise: Promise<DaemonClient> | null = null;
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
    if (this.client) return this.client;
    if (this.startPromise) return this.startPromise;

    this.startPromise = this.startDaemon().finally(() => {
      this.startPromise = null;
    });
    return this.startPromise;
  }

  async ping(): Promise<void> {
    const client = await this.getClient();
    await client.ping();
  }

  async prepareShutdown(timeoutMs = 5000): Promise<void> {
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
    this.dispose();
  }

  dispose(): void {
    this.disposed = true;
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
        AM_DATA_DIR: dataDir,
        AM_DAEMON_PORT: "0",
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

    const endpoint = await waitForEndpoint(endpointPath, () => {
      if (child.exitCode !== null) {
        throw new Error(`am-daemon exited before writing endpoint file (code ${child.exitCode})`);
      }
    });
    const client = await DaemonClient.connect(endpoint.port, endpoint.token);
    client.on("event", (event: AppEvent) => this.events.fire(event));
    client.on("event_gap", (gap: unknown) =>
      this.events.fire({ type: "event_gap", data: gap } as AppEvent),
    );
    client.on("disconnect", (err: Error) => {
      if (!this.disposed) this.output.appendLine(`[daemon] disconnected: ${err.message}`);
      if (this.client === client) this.client = null;
    });
    this.client = client;
    return client;
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
      .getConfiguration("agentmanager")
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

    const devBinary = path.resolve(
      this.context.extensionPath,
      "..",
      "target",
      "release",
      binaryName()
    );
    if (fs.existsSync(devBinary)) return devBinary;

    throw new Error(
      `No bundled am-daemon binary found for ${target}. Run npm run copy-daemon or set agentmanager.daemonPath.`
    );
  }
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
