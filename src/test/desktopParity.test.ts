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
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => {
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

test("an older background probe cannot undo an acknowledged account change", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  let finishProbe!: (value: any[]) => void;
  const pendingProbe = new Promise<any[]>((resolve) => { finishProbe = resolve; });
  const updated = { ...account, label: "Latest label" };
  let probes = 0;
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => {
    if (method === "providerAccountStatuses") return ++probes === 1 ? pendingProbe : [updated];
    if (method === "getLimitPolicy") return { accounts: [updated] };
    return [];
  } });
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  controller.refresh = async () => undefined;
  controller.detectionCache = { at: 0, providerAccounts: [account], state: "ready" };
  const detection = controller.runDetection(api);
  await controller.refreshProviderAccounts();
  finishProbe([account]);
  const result = await detection;
  assert.equal(result.providerAccounts[0].label, "Latest label");
  assert.equal(controller.detectionCache.limitPolicy.accounts[0].label, "Latest label");
  controller.dispose();
});

test("tool groups stop at messages and separate turns without losing event order", async () => {
  const { groupToolRuns } = await bundle("webview/src/transcript.ts");
  const event = (id: string, kind: string, turn_id = "turn") => ({ type: "event", event: {id, kind, turn_id} });
  const blocks = groupToolRuns([event("a", "tool_call"), event("b", "tool_result"), event("c", "message"), event("d", "file_changed"), event("e", "tool_call", "next-turn")]);
  assert.deepEqual(blocks.map((block: any) => block.type), ["tools", "event", "tools", "tools"]);
  assert.deepEqual(blocks[0].events.map((item: any) => item.id), ["a", "b"]);
});

test("provider plans use readable desktop labels", async () => {
  const { planName } = await bundle("webview/src/accounts.tsx");
  for (const value of ["prolite", "PROLITE", "pro_lite", "pro-lite", "Pro Lite"]) assert.equal(planName(value), "Pro Lite");
  assert.equal(planName("pro"), "Pro");
  assert.equal(planName("free"), "Free");
  assert.equal(planName("new_plan"), "New Plan");
  assert.equal(planName(null), null);
});

test("paused local and Docker features reject operations without probing or contacting providers", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const called: string[] = [];
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => { called.push(String(method)); return []; } });
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  await controller.runDetection(api);
  assert.ok(!called.includes("detectLocalModels"));
  assert.ok(!called.includes("detectSandboxRuntime"));
  called.length = 0;
  for (const type of ["setSandboxPolicy", "sandboxLogin", "setLocalModelPolicy"]) {
    const replies: any[] = [];
    await controller.handleMessage({ type, requestId: type }, (message: any) => replies.push(message));
    assert.ok(replies.some((message) => message.type === "operationResult" && /temporarily unavailable/.test(message.error)));
  }
  assert.equal(called.length, 0);
  controller.dispose();
});

test("notification history deduplicates failures and retains unresolved errors through success updates", async () => {
  const { appendNotification } = await bundle("webview/src/notifications.tsx");
  let items = appendNotification([], "Cannot save settings", true, 1);
  items = appendNotification(items, "Connected repository", false, 2);
  assert.equal(appendNotification(items, "Cannot save settings", true, 3), items);
  for (let id = 4; id < 30; id++) items = appendNotification(items, `Completed action ${id}`, false, id);
  assert.equal(items.length, 12);
  assert.ok(items.some((item: any) => item.message === "Cannot save settings" && !item.dismissed));
  items = items.map((item: any) => ({ ...item, dismissed: true }));
  assert.equal(appendNotification(items, "Cannot save settings", true, 31)[0].id, 31);
  const errors = Array.from({ length: 12 }, (_, id) => ({ id, message: `Failure ${id}`, error: true, dismissed: false }));
  assert.equal(appendNotification(errors, "New failure", true, 13)[0].message, "New failure");
});

test("a failed message transport rejects cleanly and can recover on the next action", async () => {
  const listeners: ((event: any) => void)[] = [];
  (globalThis as any).window = { addEventListener: (_: string, callback: any) => listeners.push(callback), setTimeout };
  const { configureTransport, request } = await bundle("webview/src/bridge.ts");
  configureTransport(() => { throw new Error("Extension connection closed"); });
  await assert.rejects(request({ type: "refresh" }), /connection closed/);
  configureTransport((message: any) => listeners[0]({ data: {type: "operationResult", requestId: message.requestId, error: null} }));
  await request({ type: "refresh" });
});
