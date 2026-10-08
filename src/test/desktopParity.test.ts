import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

async function bundle(entry: string, stubVscode = false): Promise<any> {
  const dir = mkdtempSync(path.join(tmpdir(), "perpetual-parity-"));
  const outfile = path.join(dir, "module.cjs");
  const stub = path.join(dir, "vscode.cjs");
  if (stubVscode) writeFileSync(stub, `
    class EventEmitter {
      listeners = []; event = (fn) => { this.listeners.push(fn); return {dispose(){}}; };
      fire(value) { for (const fn of this.listeners) fn(value); } dispose() {}
    }
    const listener = () => ({dispose(){}});
    module.exports = { EventEmitter, workspace: { isTrusted: true, onDidGrantWorkspaceTrust: listener, onDidChangeWorkspaceFolders: listener, onDidChangeConfiguration: listener }, window: { onDidChangeWindowState: listener }, env: {} };
  `);
  await build({ entryPoints: [path.resolve(entry)], outfile, bundle: true, platform: "node", format: "cjs", alias: stubVscode ? { vscode: stub } : undefined, logLevel: "silent" });
  const loaded = require(outfile);
  rmSync(dir, { recursive: true, force: true });
  return loaded;
}

const account = { id: "codex-personal", label: "Personal", agent: "codex", enabled: true, use_credits: false, auth_mode: "system", installed: true, authenticated: true, active: true, availability: "available", reset_at: null, detail: null };

test("account states distinguish missing tools, paused rotation, sign-in, limits and active selection", async () => {
  const { accountState, accountName } = await bundle("webview/src/accounts.tsx");
  assert.equal(accountState(account), "active");
  assert.equal(accountState({ ...account, installed: false }), "missing");
  assert.equal(accountState({ ...account, enabled: false }), "paused");
  assert.equal(accountState({ ...account, authenticated: false }), "signed-out");
  assert.equal(accountState({ ...account, availability: "limited" }), "limited");
  assert.equal(accountState({ ...account, active: false }), "ready");
  assert.equal(accountName({ ...account, email: "work@example.test" }), "work@example.test");
});

test("host normalization preserves system sign-ins and dismissed shared profiles", async () => {
  const { normalizeLimitPolicy } = await bundle("src/node/workbenchController.ts", true);
  const normalized = normalizeLimitPolicy({ accounts: [account], dismissed_system_accounts: ["claude_code"], agent_priority: ["codex", "claude_code"], unknown_reset_retry_secs: 600 });
  assert.equal(normalized.accounts[0].auth_mode, "system");
  assert.deepEqual(normalized.dismissed_system_accounts, ["claude_code"]);
});

test("account mutations serialize and merge rapid edits against the latest persisted policy", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  let policy = { accounts: [{ ...account }, { ...account, id: "codex-work", label: "Work", active: false }], agent_priority: ["codex", "claude_code"], unknown_reset_retry_secs: 600 };
  const api = {
    getLimitPolicy: async () => structuredClone(policy),
    setLimitPolicy: async (next: any) => { await new Promise((resolve) => setTimeout(resolve, 5)); policy = structuredClone(next); return policy; },
    providerAccountStatuses: async () => policy.accounts,
  };
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  controller.refresh = async () => undefined;
  const replies: any[] = [];
  await Promise.all([
    controller.handleMessage({ type: "updateProviderAccount", accountId: account.id, patch: { label: "Renamed" }, requestId: "name" }, (result: any) => replies.push(result)),
    controller.handleMessage({ type: "updateProviderAccount", accountId: account.id, patch: { enabled: false }, requestId: "pause" }, (result: any) => replies.push(result)),
    controller.handleMessage({ type: "updateProviderAccount", accountId: "codex-work", patch: { use_credits: true }, requestId: "credits" }, (result: any) => replies.push(result)),
  ]);
  assert.equal(policy.accounts[0].label, "Renamed");
  assert.equal(policy.accounts[0].enabled, false);
  assert.equal(policy.accounts[0].auth_mode, "system");
  assert.equal(policy.accounts[1].use_credits, true);
  assert.deepEqual(replies.filter((result) => result.type === "operationResult").map((result) => [result.requestId, result.error]), [["name", null], ["pause", null], ["credits", null]]);
  await controller.handleMessage({ type: "reorderProviderAccounts", orderedIds: ["codex-work"], requestId: "stale" }, (result: any) => replies.push(result));
  assert.match(replies.find((result) => result.requestId === "stale").error, /Accounts changed/);
  assert.equal(policy.accounts.length, 2);
  controller.dispose();
});

test("switching cannot activate an account with a missing CLI or expired sign-in", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  let activated = false;
  const api = { providerAccountStatuses: async () => [{ ...account, installed: false }], activateProviderAccount: async () => { activated = true; } };
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  controller.refresh = async () => undefined;
  const replies: any[] = [];
  await controller.handleMessage({ type: "activateProviderAccount", accountId: account.id, requestId: "switch" }, (result: any) => replies.push(result));
  assert.equal(activated, false);
  assert.match(replies.find((result) => result.type === "operationResult").error, /installed provider/);
  controller.dispose();
});

test("webview actions wait for matching acknowledgements and propagate save errors", async () => {
  const listeners: ((event: any) => void)[] = [];
  (globalThis as any).window = { addEventListener: (_: string, callback: any) => listeners.push(callback), setTimeout };
  const { configureTransport, request } = await bundle("webview/src/bridge.ts");
  const sent: any[] = [];
  configureTransport((message: any) => sent.push(message));
  let finished = false;
  const first = request({ type: "updateProviderAccount" }).then(() => { finished = true; });
  await Promise.resolve();
  assert.equal(finished, false);
  listeners[0]({ data: { type: "operationResult", requestId: "unrelated", error: null } });
  assert.equal(finished, false);
  listeners[0]({ data: { type: "operationResult", requestId: sent[0].requestId, error: null } });
  await first;
  assert.equal(finished, true);
  const second = request({ type: "setProviderAccountToken" });
  const rejected = assert.rejects(second, /Could not save token/);
  listeners[0]({ data: { type: "operationResult", requestId: sent[1].requestId, error: "Could not save token" } });
  await rejected;
});

test("delayed history cannot rewind streamed text or revive completed messages", async () => {
  const { mergeThreadEvents } = await bundle("webview/src/streaming.ts");
  const streaming = { id: "reply", text: "A complete sentence", data: { streaming: true } };
  const delayed = { ...streaming, text: "A complete" };
  assert.equal(mergeThreadEvents([streaming], [delayed])[0].text, streaming.text);
  const completed = { ...streaming, data: { streaming: false } };
  const next = mergeThreadEvents([completed], [streaming]);
  assert.equal(next[0].data.streaming, false);
  const untouched = { id: "other", text: "Earlier message", data: {} };
  assert.equal(mergeThreadEvents([untouched, streaming], [completed])[0], untouched);
});

test("run picker collapses authenticated identities without deleting separate profiles", async () => {
  const { uniqueAccountChoices } = await bundle("webview/src/accounts.tsx");
  const input = [
    { ...account, email: "person@example.test" },
    { ...account, id: "isolated", auth_mode: "isolated_cli", active: false, email: " Person@example.test " },
    { ...account, id: "other-provider", agent: "claude_code", email: "person@example.test" },
    { ...account, id: "signed-out", active: false, authenticated: false, email: "person@example.test" },
  ];
  assert.deepEqual(uniqueAccountChoices(input).map((item: any) => item.id), [account.id, "other-provider", "signed-out"]);
  assert.equal(input.length, 4);
  const ready = { ...input[1], availability: "available" };
  assert.equal(uniqueAccountChoices([{ ...input[0], active: false, availability: "limited" }, ready])[0].id, "isolated");
});

test("refresh keeps accounts and models visible through invalidation and failed probes", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  const previous = { at: Date.now(), providerAccounts: [account], modelCatalog: [{agent: "codex", models: []}], agents: [], runDefaults: [], limitPolicy: { accounts: [account] }, state: "ready" };
  controller.detectionCache = previous;
  controller.invalidateDetection();
  assert.deepEqual(controller.detectionCache.providerAccounts, [account]);
  assert.equal(controller.detectionCache.at, 0);
  const api = new Proxy({}, { get: (_, method) => async () => {
    if (["providerAccountStatuses", "agentModelCatalog", "getLimitPolicy"].includes(String(method))) throw new Error("transient probe failure");
    return [];
  } });
  const result = await controller.runDetection(api);
  assert.deepEqual(result.providerAccounts, [account]);
  assert.deepEqual(result.modelCatalog, previous.modelCatalog);
  assert.deepEqual(result.limitPolicy, previous.limitPolicy);
  controller.dispose();
});

test("desktop tool trace pairs call ids, records failures, and collects file edits", async () => {
  const { toolRun } = await bundle("webview/src/ai.tsx");
  const event = (id: string, kind: string, text: string, data: any = {}) => ({ id, kind, text, data });
  const result = toolRun([
    event("read", "tool_call", "Read", { call_id: "read-id", input: { file_path: "app.ts" } }),
    event("run", "tool_call", "exec", { call_id: "run-id", input: { command: "pwsh -Command 'npm test'" } }),
    event("result", "tool_result", "Failed test", { call_id: "run-id", ok: false }),
    event("file", "file_changed", "Modified app.ts"),
    event("message", "message", "Done"),
  ]);
  assert.equal(result.steps.length, 2);
  assert.equal(result.steps[0].done, false);
  assert.equal(result.steps[1].done, true);
  assert.equal(result.steps[1].failed, true);
  assert.equal(result.steps[1].chip, "npm test");
  assert.deepEqual(result.files, [{path: "app.ts", change: "modified"}]);
});

test("tool groups stop at messages and separate turns without losing event order", async () => {
  const { groupToolRuns } = await bundle("webview/src/transcript.ts");
  const event = (id: string, kind: string, turn_id = "turn") => ({ type: "event", event: {id, kind, turn_id} });
  const blocks = groupToolRuns([event("a", "tool_call"), event("b", "tool_result"), event("c", "message"), event("d", "file_changed"), event("e", "tool_call", "next-turn")]);
  assert.deepEqual(blocks.map((block: any) => block.type), ["tools", "event", "tools", "tools"]);
  assert.deepEqual(blocks[0].events.map((item: any) => item.id), ["a", "b"]);
});
