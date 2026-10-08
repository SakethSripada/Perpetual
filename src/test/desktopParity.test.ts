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
