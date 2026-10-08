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
    module.exports = { EventEmitter, extensions: {getExtension: () => globalThis.__perpetualTestGitExtension}, workspace: { isTrusted: true, getConfiguration: () => ({get: (_, fallback) => fallback, inspect: () => undefined}), onDidGrantWorkspaceTrust: listener, onDidChangeWorkspaceFolders: listener, onDidChangeConfiguration: listener }, window: { onDidChangeWindowState: listener }, env: {} };
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

test("switching rejects limited, missing and signed-out accounts, then recovers for a ready account", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  let selected: any = { ...account, availability: "limited" }, activations = 0, modelProbes = 0;
  const api = new Proxy({ providerAccountStatuses: async () => selected ? [selected] : [], activateProviderAccount: async () => { activations++; }, getLimitPolicy: async () => ({ accounts: [selected] }), agentModelCatalog: async () => { modelProbes++; return []; } }, { get: (target, method) => method in target ? target[method as keyof typeof target] : method === "then" ? undefined : async () => [] });
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  controller.refresh = async () => undefined;
  const errors: (string | null)[] = [];
  const switchAccount = () => controller.handleMessage({ type: "activateProviderAccount", accountId: account.id, requestId: "switch" }, (result: any) => { if (result.type === "operationResult") errors.push(result.error); });
  await switchAccount(); selected = null; await switchAccount();
  selected = { ...account, authenticated: false }; await switchAccount();
  assert.equal(activations, 0);
  assert.match(errors[0]!, /usage limit/);
  assert.ok(errors.every(Boolean));
  controller.detectionCache = { at: Date.now(), state: "ready", providerAccounts: [account] };
  selected = { ...account }; await switchAccount();
  assert.equal(activations, 1);
  assert.equal(errors.at(-1), null);
  assert.equal(modelProbes, 1, "switching immediately rechecks model entitlements");
  assert.equal(controller.detectionCache.providerAccounts[0].id, account.id, "visible accounts stay loaded");
  controller.dispose();
});

test("concurrent refresh requests share reads and reconcile once more after pending changes", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  let release!: () => void, reads = 0, active = 0, maximum = 0;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  controller.snapshot = async () => { reads++; maximum = Math.max(maximum, ++active); if (reads === 1) await gate; active--; return { threads: [{id: String(reads)}] }; };
  const published: any[] = []; controller.onSnapshot((value: any) => published.push(value));
  const requests = Array.from({ length: 25 }, () => controller.refresh());
  assert.equal(reads, 1); release(); await Promise.all(requests);
  assert.equal(reads, 2); assert.equal(maximum, 1);
  assert.equal(published.at(-1).threads[0].id, "2");
  controller.dispose(); await controller.refresh(); assert.equal(reads, 2);
});

test("accounts and their discovered policy publish before a slow model catalog finishes", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  let release!: (models: any[]) => void;
  const models = new Promise<any[]>((resolve) => { release = resolve; });
  let policies = 0;
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => {
    if (method === "agentModelCatalog") return models;
    if (method === "providerAccountStatuses") return [account];
    if (method === "getLimitPolicy") return { accounts: ++policies === 1 ? [] : [account] };
    return [];
  } });
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  controller.refresh = async () => undefined;
  const pending = controller.runDetection(api);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(controller.detectionCache.accountState, "ready");
  assert.equal(controller.detectionCache.providerAccounts[0].id, account.id);
  assert.equal(controller.detectionCache.limitPolicy.accounts[0].id, account.id);
  assert.equal(controller.detectionCache.state, "loading");
  release([]); const result = await pending;
  assert.equal(result.limitPolicy.accounts[0].id, account.id);
  assert.equal(result.modelState, "ready");
  controller.dispose();
});

test("fresh detection is reused and invalidation shares one pending probe", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  let probes = 0, release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  controller.refresh = async () => undefined;
  controller.detectionCache = { at: Date.now(), state: "ready", providerAccounts: [account] };
  controller.runDetection = () => { probes++; controller.detectInflight = gate.finally(() => { controller.detectInflight = null; }); return controller.detectInflight; };
  for (let index = 0; index < 50; index++) controller.detect({});
  assert.equal(probes, 0);
  controller.invalidateDetection(); controller.detect({}); controller.detect({});
  assert.equal(probes, 1); release(); await gate; controller.dispose();
});

test("progressive discovery patches loaded resources without reading or transferring conversation history", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  const details = { events: [{id: "retained-message"}] };
  controller.lastSnapshot = { trusted: true, threads: [{id: "retained-chat"}], details };
  let reads = 0;
  controller.refresh = async () => { reads++; };
  const updates: any[] = []; controller.onDetectionUpdate((patch: any) => updates.push(patch));
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => method === "providerAccountStatuses" ? [account] : method === "getLimitPolicy" ? {accounts: [account]} : [] });
  await controller.runDetection(api); controller.publishDetection();
  assert.equal(reads, 0);
  assert.ok(updates.some((patch) => patch.providerAccounts[0]?.id === account.id));
  assert.ok(updates.every((patch) => !("details" in patch) && !("threads" in patch)));
  assert.equal(controller.lastSnapshot.details, details);
  assert.equal(controller.lastSnapshot.detectionState, "ready");
  controller.dispose();
});

test("invalidation during model discovery discards old entitlements and schedules a fresh probe", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  let release!: (models: any[]) => void, probes = 0;
  const oldModels = new Promise<any[]>((resolve) => { release = resolve; });
  const currentModels = [{agent: "codex", models: [{id: "new-model"}]}];
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => method === "agentModelCatalog" ? (++probes === 1 ? oldModels : currentModels) : method === "getLimitPolicy" ? {accounts: []} : [] });
  controller.detect(api); const oldProbe = controller.detectInflight;
  controller.invalidateDetection();
  release([{agent: "codex", models: [{id: "old-model"}]}]);
  await oldProbe;
  await new Promise((resolve) => setImmediate(resolve));
  if (controller.detectInflight) await controller.detectInflight;
  assert.equal(probes, 2);
  assert.equal(controller.detectionCache.modelCatalog[0].models[0].id, "new-model");
  assert.equal(controller.detectionCache.modelState, "ready");
  controller.dispose();
});

test("token bursts bypass workspace reads but completion reconciles durable state", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  let forwarded = 0, reads = 0; controller.onThreadEvent(() => forwarded++);
  controller.refresh = async () => { reads++; };
  for (let index = 0; index < 100; index++) controller.onDaemonEvent({ type: "agent_thread_event", data: { id: "reply", thread_id: "thread", data: {streaming: true}, text: String(index) } });
  assert.equal(forwarded, 100); assert.equal(reads, 0); assert.equal(controller.refreshTimer, null);
  controller.onDaemonEvent({ type: "agent_thread_event", data: { id: "reply", thread_id: "thread", data: {streaming: false} } });
  assert.ok(controller.refreshTimer); controller.dispose();
});

test("frame batching delivers the latest text once and never rewinds a completion", async () => {
  const { createEventBatcher } = await bundle("webview/src/streaming.ts");
  let scheduled = 0; const deliveries: any[][] = [];
  const batch = createEventBatcher((events: any[]) => deliveries.push(events), () => ++scheduled, () => {});
  for (let index = 0; index < 100; index++) batch.enqueue({id: "reply", text: "x".repeat(index), data: { streaming: true }});
  assert.equal(scheduled, 1); assert.equal(deliveries.length, 0);
  batch.enqueue({id: "reply", text: "Finished", data: {streaming: false}});
  batch.enqueue({id: "reply", text: "Late text", data: {streaming: true}});
  batch.flush(); assert.equal(deliveries[0].length, 1); assert.equal(deliveries[0][0].text, "Finished");
  batch.enqueue({id: "other", text: "Cancelled"}); batch.dispose(); batch.flush(); assert.equal(deliveries.length, 1);
});

test("automatic account transitions refresh account state without forcing model probes", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}) }, { appendLine() {} });
  let checks = 0, release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  controller.refreshProviderAccounts = async () => { checks++; await gate; };
  controller.lastSnapshot = { threads: [{ id: "thread", status: "running", provider_account_id: "old" }] };
  controller.onDaemonEvent({ type: "agent_thread_updated", data: {id: "thread", status: "paused", handoff_state: "budget_paused_provider_limit", provider_account_id: "old"} });
  assert.equal(checks, 1, "percentage-budget limit pauses refresh account availability too");
  controller.onDaemonEvent({ type: "agent_thread_updated", data: {id: "thread", status: "running", provider_account_id: "new"} });
  assert.equal(checks, 1); assert.equal(controller.detectInflight, null);
  release(); await controller.accountEventRefresh; assert.equal(checks, 2); controller.dispose();
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

test("conversation search matches original requests and ignores extra whitespace", async () => {
  const { searchSessions } = await bundle("webview/src/sessions.tsx");
  const threads = [{ id: "one", title: "Fix login", objective: "Repair OAuth refresh" }, { id: "two", title: "Dashboard", objective: "Add widgets" }];
  assert.deepEqual(searchSessions(threads, "  FIX   oauth ").map((item: any) => item.id), ["one"]);
  assert.equal(searchSessions(threads, "missing").length, 0);
  assert.equal(searchSessions(threads, "  ").length, 2);
});

test("resource loading never presents pending or failed probes as confirmed empty", async () => {
  const { resourceState } = await bundle("webview/src/loading.tsx");
  const empty = { loadState: "ready", detectionState: "loading", threads: [], repos: [], providerAccounts: [], modelCatalog: [], error: null };
  assert.equal(resourceState(null, "threads"), "loading");
  assert.equal(resourceState(empty, "accounts"), "loading");
  assert.equal(resourceState(empty, "models"), "loading");
  assert.equal(resourceState(empty, "threads"), "ready");
  assert.equal(resourceState({ ...empty, detectionState: "error" }, "accounts"), "error");
  assert.equal(resourceState({ ...empty, loadState: "error" }, "threads"), "error");
  assert.equal(resourceState({ ...empty, loadState: "error", providerAccounts: [account] }, "accounts"), "ready");
  assert.equal(resourceState({ ...empty, detectionState: "ready" }, "accounts"), "ready");
  assert.equal(resourceState({ ...empty, accountDetectionState: "ready", modelDetectionState: "loading" }, "accounts"), "ready");
  assert.equal(resourceState({ ...empty, accountDetectionState: "ready", modelDetectionState: "loading" }, "models"), "loading");
});

test("a failed refresh preserves the loaded workspace instead of publishing empty lists", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getClient: async () => { throw new Error("Daemon disconnected"); } }, { appendLine() {} });
  controller.lastSnapshot = { trusted: true, threads: [{id: "existing"}], repos: [{id: "repo"}], providerAccounts: [account], selectedThreadId: "existing", detectionState: "ready" };
  const result = await controller.snapshot(null);
  assert.equal(result.loadState, "error");
  assert.equal(result.threads[0].id, "existing");
  assert.equal(result.repos[0].id, "repo");
  assert.equal(result.providerAccounts[0].id, account.id);
  assert.match(result.error, /Daemon disconnected/);
  controller.dispose();
});

test("unified change previews preserve hunk numbers, header-like code and deleted file paths", async () => {
  const { parsePatch } = await bundle("webview/src/changeLog.tsx");
  const files = parsePatch('diff --git a/example.ts b/example.ts\n--- a/example.ts\n+++ b/example.ts\n@@ -10,2 +20,2 @@\n context\n-old\n+++new\ndiff --git a/old.ts b/old.ts\n--- a/old.ts\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n');
  assert.deepEqual(files.get("example.ts").slice(1), [
    { old: 10, current: 20, kind: "context", text: "context" },
    { old: 11, current: null, kind: "del", text: "old" },
    { old: null, current: 21, kind: "add", text: "++new" },
  ]);
  assert.equal(files.get("old.ts")[1].text, "gone");
  assert.equal(parsePatch('--- "a/a file.ts"\n+++ "b/a file.ts"\n@@ -0,0 +1 @@\n+first').get("a file.ts")[1].current, 1);
});

test("paused local and Docker features reject operations without probing or contacting providers", async () => {
  const { WorkbenchController } = await bundle("src/node/workbenchController.ts", true);
  const called: string[] = [];
  const api = new Proxy({}, { get: (_, method) => method === "then" ? undefined : async () => { called.push(String(method)); return []; } });
  const controller = new WorkbenchController({ subscriptions: [] }, { onEvent: () => ({dispose(){}}), getLocalClient: async () => api }, { appendLine() {} });
  controller.refresh = async () => undefined;
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

test("usage windows preserve partial reports and reject invalid values without inventing quotas", async () => {
  const { usageWindows } = await bundle("webview/src/usageData.ts");
  assert.deepEqual(usageWindows(null), []);
  assert.deepEqual(usageWindows({five_hour: null, weekly: null}), []);
  const weekly = usageWindows({five_hour: null, weekly: {used_percent: 10, reset_at: "2026-10-10T12:00:00Z"}});
  assert.equal(weekly.length, 1);
  assert.equal(weekly[0].key, "weekly");
  assert.equal(weekly[0].remaining, 90);
  assert.equal(weekly[0].reset.toISOString(), "2026-10-10T12:00:00.000Z");
  assert.equal(usageWindows({weekly: {used_percent: 140, reset_at: "invalid"}})[0].remaining, 0);
  assert.equal(usageWindows({weekly: {used_percent: -5, reset_at: null}})[0].remaining, 100);
  assert.equal(usageWindows({weekly: {used_percent: 5, reset_at: "invalid"}})[0].reset, null);
  assert.deepEqual(usageWindows({weekly: {used_percent: NaN, reset_at: null}}), []);
  assert.deepEqual(usageWindows({weekly: {used_percent: Infinity, reset_at: null}}), []);
});

test("model selection resolves real provider defaults and only supported reasoning levels", async () => {
  const {resolveModelSelection, readModelSelections} = await bundle("webview/src/modelSelection.ts");
  const snapshot = { runDefaults: [{kind: "codex", model: "config-model", reasoning: "low"}], limitPolicy: {agent_profiles: [{agent: "codex", model: null, reasoning: null}]}, modelCatalog: [{agent: "codex", default_model: "catalog-model", default_reasoning: "medium", reasoning: ["low", "medium"], models: [{id: "config-model", label: "Configured model", aliases: ["configured"], available: true, reasoning: ["low", "high"], default_reasoning: "high"}, {id: "quiet", label: "Quiet model", available: true, aliases: [], reasoning: [], default_reasoning: null}]}] };
  const resolved = resolveModelSelection(snapshot, "codex");
  assert.equal(resolved.model, "config-model");
  assert.equal(resolved.modelLabel, "Configured model");
  assert.equal(resolved.reasoning, "high");
  assert.equal(resolveModelSelection(snapshot, "codex", "configured", "HIGH").reasoning, "high");
  assert.equal(resolveModelSelection(snapshot, "codex", "quiet", "high").reasoning, "");
  assert.equal(resolveModelSelection(snapshot, "claude_code").model, "");
  assert.equal(resolveModelSelection(snapshot, "codex", "future-custom", "low").model, "future-custom");
  assert.deepEqual(readModelSelections({lastAgent: "codex", lastModel: "config-model", lastReasoning: "high"}), {codex: {model: "config-model", reasoning: "high"}});
  assert.deepEqual(readModelSelections({modelSelections: {}, lastAgent: "codex", lastModel: "config-model"}), {});
  assert.deepEqual(readModelSelections({modelSelections: {codex: {model: 1, reasoning: "high"}, claude_code: {model: "claude-sonnet-5", reasoning: "high"}}}), {claude_code: {model: "claude-sonnet-5", reasoning: "high"}});
});

test("model preferences persist per provider and publish without workspace reads", async () => {
  const {WorkbenchController} = await bundle("src/node/workbenchController.ts", true);
  const saved = new Map();
  const controller = new WorkbenchController({subscriptions: [], workspaceState: {get: (key: string) => saved.get(key), update: async (key: string, value: any) => {await new Promise(resolve => setTimeout(resolve, 5)); saved.set(key, value);}}}, {onEvent: () => ({dispose(){}})}, {appendLine(){}});
  controller.refresh = async () => {throw new Error("model selection must not fetch workspace");};
  const patches: any[] = []; controller.onDetectionUpdate((patch: any) => patches.push(patch));
  await Promise.all([controller.handleMessage({type: "setModelSelection", agent: "codex", model: "gpt-current", reasoning: "high"}), controller.handleMessage({type: "setModelSelection", agent: "claude_code", model: "claude-current", reasoning: "low"})]);
  assert.deepEqual(saved.get("perpetual.modelSelections"), {codex: {model: "gpt-current", reasoning: "high"}, claude_code: {model: "claude-current", reasoning: "low"}});
  assert.equal(patches.length, 2); controller.dispose();
});

test("review refresh coalesces Git bursts and reconciles changes arriving in flight", async () => {
  const {WorkbenchController} = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({subscriptions: []}, {onEvent: () => ({dispose(){}})}, {appendLine(){}});
  let finish!: () => void; let reads = 0;
  controller.readDiff = async () => {reads++; if (reads === 1) await new Promise<void>(resolve => {finish = resolve;});};
  const first = controller.loadDiff("thread");
  const second = controller.loadDiff("thread");
  const third = controller.loadDiff("thread");
  assert.equal(reads, 1); finish(); await Promise.all([first, second, third]);
  assert.equal(reads, 2); controller.dispose();
});

test("Git review watching refreshes only an open visible-repository review", async () => {
  let change!: () => void;
  const repo = {rootUri: {fsPath: "C:/project"}, state: {onDidChange: (callback: () => void) => {change = callback; return {dispose(){}};}}};
  (globalThis as any).__perpetualTestGitExtension = {activate: async () => ({getAPI: () => ({repositories: [repo], onDidOpenRepository: () => ({dispose(){}})})})};
  const {WorkbenchController} = await bundle("src/node/workbenchController.ts", true);
  const controller = new WorkbenchController({subscriptions: []}, {onEvent: () => ({dispose(){}})}, {appendLine(){}});
  try {
    await Promise.resolve(); await Promise.resolve();
    let reads = 0; controller.loadDiff = async () => {reads++;};
    controller.lastSnapshot = {repos: [{id: "repo", local_path: "C:/project"}], details: {repos: [{repo_id: "repo", worktree_path: "C:/project"}]}};
    change(); await new Promise(resolve => setTimeout(resolve, 300)); assert.equal(reads, 0);
    await controller.handleMessage({type: "reviewVisibility", threadId: "thread", visible: true});
    change(); change(); await new Promise(resolve => setTimeout(resolve, 300)); assert.equal(reads, 1);
    controller.lastSnapshot.details.repos[0].worktree_path = "C:/isolated-task";
    change(); await new Promise(resolve => setTimeout(resolve, 300)); assert.equal(reads, 1);
    await controller.handleMessage({type: "reviewVisibility", threadId: "thread", visible: false});
    change(); await new Promise(resolve => setTimeout(resolve, 300)); assert.equal(reads, 1);
  } finally {controller.dispose(); delete (globalThis as any).__perpetualTestGitExtension;}
});
