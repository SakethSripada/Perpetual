import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { once } from "node:events";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { currentTarget } from "./daemon-targets.mjs";

// Exercises the packaged binary and TypeScript wire client in disposable storage.
// No model turns, login changes, or writes to the user's Perpetual data.
const require = createRequire(import.meta.url);
const { DaemonClient } = require("../out/node/daemonClient.js");
const dataDir = await mkdtemp(path.join(os.tmpdir(), "perpetual-daemon-smoke-"));
const binary = path.resolve("bin", currentTarget(), process.platform === "win32" ? "am-daemon.exe" : "am-daemon");
let child, client;
async function connect() {
  child = spawn(binary, [], { windowsHide: true, stdio: "ignore", env: { ...process.env, PERPETUAL_DATA_DIR: dataDir, PERPETUAL_DAEMON_PORT: "0" } });
  const deadline = Date.now() + 30_000;
  let endpoint;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`Daemon exited: ${child.exitCode}`);
    try { endpoint = JSON.parse(await readFile(path.join(dataDir, "daemon.json"), "utf8")); break; } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.ok(endpoint, "daemon must publish an endpoint");
  client = await DaemonClient.connect(endpoint.port, endpoint.token);
  await client.ping();
}
async function stop() {
  client?.dispose();
  if (child && child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
  await rm(path.join(dataDir, "daemon.json"), { force: true });
}
try {
  await connect();
  const first = await client.createAgentThread({ title: "Smoke one", preferred_agent: "codex" });
  const second = await client.createAgentThread({ title: "Smoke two", preferred_agent: "claude_code" });
  assert.equal(first.execution_backend, "host");
  await client.updateAgentThread(first.id, { title: "Renamed smoke session" });
  await client.reorderAgentThreads([second.id, first.id]);
  const ordered = await client.listAgentThreads();
  assert.deepEqual(ordered.map((thread) => thread.id), [second.id, first.id]);
  const accounts = await client.providerAccountStatuses();
  assert.ok(accounts.every((account) => typeof account.active === "boolean" && typeof account.installed === "boolean"));
  const catalog = await client.agentModelCatalog();
  assert.ok(catalog.some((provider) => provider.models.length > 0), "model catalog must contain discovered or fallback models");
  console.log(`Packaged daemon: ping, migrations, create, rename, reorder, account status, model discovery passed (${accounts.length} profiles; ${catalog.length} providers).`);
  await stop(); await connect();
  const persisted = await client.listAgentThreads();
  assert.deepEqual(persisted.map((thread) => thread.id), [second.id, first.id]);
  assert.equal(persisted[1].title, "Renamed smoke session");
  console.log("Session name and order persisted across daemon restart.");
} finally {
  await stop();
  assert.equal(path.dirname(path.resolve(dataDir)), path.resolve(os.tmpdir()));
  assert.ok(path.basename(dataDir).startsWith("perpetual-daemon-smoke-"));
  await rm(dataDir, { recursive: true, force: true });
}
