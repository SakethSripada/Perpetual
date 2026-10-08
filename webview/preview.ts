/** Local fixture for visual QA. Never imported by the extension entry point. */
import type { WorkbenchSnapshot, ProviderAccountStatus, AgentThread, ExtensionMessage } from "./src/types";
const params = new URLSearchParams(location.search);
const light = params.get("theme") === "light";
document.body.className = light ? "vscode-light" : "vscode-dark";
const tokens = {
  "editor-background": light ? "#fcfbf9" : "#1f1f1f", "sideBar-background": light ? "#f3f2ef" : "#181818",
  "editorWidget-background": light ? "#ffffff" : "#282828", foreground: light ? "#1f1f1e" : "#ecebe8",
  descriptionForeground: light ? "#6c6a65" : "#a09f9b", "font-family": "'Segoe UI', system-ui, sans-serif",
  "button-background": light ? "#46735a" : "#b2cdbd", "button-foreground": light ? "#ffffff" : "#13201a",
  "button-hoverBackground": light ? "#3a624c" : "#c1dacb", "button-secondaryBackground": light ? "#ebeae6" : "#2e2e2e",
  "button-secondaryForeground": light ? "#1f1f1e" : "#ecebe8", "input-background": light ? "#ffffff" : "#282828",
  "input-border": light ? "#e1dfda" : "#363636", "input-foreground": light ? "#1f1f1e" : "#ecebe8",
  "panel-border": light ? "#e1dfda" : "#363636", focusBorder: light ? "#46735a" : "#b2cdbd",
  "list-hoverBackground": light ? "#ebeae6" : "#2e2e2e", "list-inactiveSelectionBackground": light ? "#e8e7e2" : "#282828",
  "errorForeground": light ? "#bf3b2f" : "#ec8b80", "testing-iconPassed": light ? "#2c8052" : "#86c79f", "charts-orange": light ? "#9a6410" : "#e3b96f",
};
for (const [name, value] of Object.entries(tokens)) document.documentElement.style.setProperty(`--vscode-${name}`, value);
if (params.has("width")) document.getElementById("root")!.style.width = `${Number(params.get("width"))}px`;
const now = new Date().toISOString();
const accounts: ProviderAccountStatus[] = [
  { id: "system-codex", label: "Codex sign-in", agent: "codex", auth_mode: "system", enabled: true, use_credits: false, email: "saketh@example.test", plan: "prolite", installed: true, authenticated: true, availability: "available", reset_at: null, detail: null, active: true },
  { id: "codex-work", label: "Work", agent: "codex", auth_mode: "isolated_cli", enabled: true, use_credits: false, email: "work@example.test", plan: "business", installed: true, authenticated: true, availability: "limited", reset_at: new Date(Date.now() + 7200000).toISOString(), detail: null, active: false },
  { id: "system-claude_code", label: "Claude sign-in", agent: "claude_code", auth_mode: "system", enabled: true, use_credits: false, email: "saketh@example.test", plan: "max", installed: true, authenticated: true, availability: "available", reset_at: null, detail: null, active: true },
];
accounts.push({ ...accounts[0], id: 'duplicate-codex', auth_mode: 'isolated_cli', label: 'Personal profile', active: false });
accounts.push({ ...accounts[2], id: 'duplicate-claude', auth_mode: 'isolated_cli', label: 'Claude profile', active: false });
function thread(id: string, title: string, status: AgentThread["status"]): AgentThread {
  return { id, title, status, active_agent: "codex", preferred_agent: "codex", permission: "workspace_write", execution_backend: "host", model: "gpt-5.5", reasoning: "medium", provider_account_id: "system-codex", task_budget: { mode: "unlimited" }, updated_at: now, created_at: now } as AgentThread;
}
let snapshot: WorkbenchSnapshot = {
  trusted: true, defaults: { agent: "codex", permission: "workspace_write", execution_backend: "host", model: null, reasoning: null, local_provider: null, local_base_url: null }, project: null,
  selectedThreadId: null, threads: [thread("session-one", "Improve the dashboard", "review"), thread("session-two", "Investigate authentication", "waiting_for_limit"), thread("session-three", "Add keyboard navigation", "running")],
  repos: [{ id: "repo-one", name: "Perpetual", project_id: "project-one", kind: "local", local_path: "C:/Development/Perpetual", default_branch: "dev", remote_url: null, created_at: now, updated_at: now }], defaultRepoIds: ["repo-one"],
  agents: ["codex", "claude_code"].map((kind) => ({ kind, installed: true, authenticated: true, version: "1.0", availability: "available", binary_path: null, reset_at: null, last_checked: now, usage: { five_hour: { used_percent: 36, reset_at: now }, weekly: { used_percent: 24, reset_at: now } } })) as WorkbenchSnapshot["agents"],
  runDefaults: [], providerAccounts: accounts, authPendingAccountIds: [], detectionState: "ready",
  modelCatalog: [ { agent: "codex", default_model: "gpt-5.5", default_reasoning: "medium", models: [{ id: "gpt-5.5", label: "GPT-5.5", aliases: [], family: "gpt", default: true, available: true, source: "codex_app_server", reasoning: ["low", "medium", "high", "xhigh"], default_reasoning: "medium" }], reasoning: ["low", "medium", "high", "xhigh"], binary_path: null, version: "1.0", source: "codex_app_server", detected_at: now, error: null } ],
  limitPolicy: { auto_switch: true, switch_back: true, agent_priority: ["codex", "claude_code"], accounts, agent_profiles: [], keep_awake: false, resume_with_earliest: true, unknown_reset_retry_secs: 600 }, sandboxPolicy: null, sandboxRuntime: null, cloudPolicy: null, cloudAvailability: [], localModelPolicy: null, localModels: [], details: null, github: null,
  collaboration: { role: "standalone", connected: false, host_name: null, device_id: "preview", device_name: "Preview", devices: [], assignments: [], change_sets: [], server_time: now }, error: null,
};
snapshot.modelCatalog!.push({agent: "claude_code", default_model: "claude-sonnet-5", default_reasoning: "high", models: [
  {id: "claude-sonnet-5", label: "Claude Sonnet 5", aliases: ["sonnet"], family: "sonnet", default: true, available: true, source: "claude_code", reasoning: ["low", "medium", "high"], default_reasoning: "high"},
  {id: "claude-haiku-4-5", label: "Claude Haiku 4.5", aliases: ["haiku"], family: "haiku", default: false, available: true, source: "claude_code", reasoning: [], default_reasoning: null}
], reasoning: ["low", "medium", "high"], binary_path: null, version: "1.0", source: "claude_code", detected_at: now, error: null});
const emit = (message: ExtensionMessage) => window.dispatchEvent(new MessageEvent("message", { data: message }));
function refresh() {
  snapshot.limitPolicy!.accounts = snapshot.providerAccounts;
  const next = structuredClone(snapshot);
  if (params.has("loading")) Object.assign(next, { loadState: "loading", detectionState: "loading", threads: [], repos: [], agents: [], providerAccounts: [], modelCatalog: [], limitPolicy: null });
  if (params.has("detection-loading")) Object.assign(next, { detectionState: "loading", agents: [], providerAccounts: [], modelCatalog: [], limitPolicy: null });
  if (params.has("load-error")) Object.assign(next, { loadState: "error", error: "Could not connect. Retry to reconnect.", threads: [], repos: [], agents: [], providerAccounts: [], modelCatalog: [], limitPolicy: null });
  if (params.has("no-repos")) Object.assign(next, { repos: [], defaultRepoIds: [] });
  if (params.has("usage-partial")) next.agents = next.agents.map((agent) => ({ ...agent, usage: agent.kind === "codex" ? { five_hour: null, weekly: { used_percent: 10, reset_at: new Date(Date.now() + 7200000).toISOString() } } : null }));
  emit({ type: "snapshot", snapshot: next });
}
const messages: any[] = [];
(window as any).previewMessages = messages;
let state: unknown = {};
let githubAttempts = 0;
window.acquireVsCodeApi = () => ({ getState: () => state, setState: (next) => { state = next; }, postMessage: (raw) => {
  const message = raw as any; messages.push(message);
  window.setTimeout(() => {
    if (message.type === "setModelSelection" && params.has("model-save-error")) {
      emit({type: "operationResult", requestId: message.requestId, error: "Could not save model selection. Try again."});
      return;
    }
    if (message.type === "setModelSelection") {
      snapshot.modelSelections = {...snapshot.modelSelections, [message.agent]: {model: message.model, reasoning: message.reasoning}};
      emit({type: "detectionUpdate", patch: {modelSelections: snapshot.modelSelections}});
    }
    if (message.type === "githubList") {
      githubAttempts++;
      if (params.has("github-error") && githubAttempts === 1) {
        emit({ type: "operationResult", requestId: message.requestId, error: "GitHub is unavailable. Try again." });
        return;
      }
      emit({ type: "githubRepos", status: null, repos: [{ id: 101, name: "Perpetual", full_name: "preview/Perpetual", private: true, html_url: "https://github.com/preview/Perpetual", clone_url: "https://github.com/preview/Perpetual.git", ssh_url: "git@github.com:preview/Perpetual.git", default_branch: "dev", updated_at: now }] });
    }
    if (message.type === "selectThread") {
      snapshot.selectedThreadId = message.threadId;
      snapshot.details = message.threadId ? { events: [ { id: "user-one", thread_id: message.threadId, turn_id: "turn-one", role: "user", kind: "message", text: "Can you improve the dashboard and make the account state clearer?", data: {}, ts: now }, { id: "assistant-one", thread_id: message.threadId, turn_id: "turn-one", role: "assistant", kind: "message", text: "I updated the dashboard with a quieter layout and clear account states.\n\n### What changed\n\n- Added an account switcher near the composer.\n- Kept account identity visible throughout the session.\n- Improved spacing, keyboard focus, and narrow panel layouts.\n\nYou can review the changes and choose which account to use for your next run.", data: {}, ts: now } ], activities: [], repos: [], turns: [], queued: [], cloudRuns: [], diff: null, approvals: [] } : null;
    }
    if (message.type === 'selectThread' && snapshot.details) {
      snapshot.details.repos = [{ repo_id: "repo-one", repo_name: "Perpetual", worktree_path: "C:/Development/Perpetual", branch: "am/thread-preview", workspace_backend: "host" }] as any;
      if (params.has("managed-review")) snapshot.details.repos[0].worktree_path = "C:/Development/.am/worktrees/preview";
      const toolEvents = [
        { id: 'call-read', thread_id: message.threadId, turn_id: 'turn-one', role: 'assistant', kind: 'tool_call', text: 'Read', data: { call_id: 'read', input: { file_path: 'src/accounts.tsx' } }, ts: now },
        { id: 'result-read', thread_id: message.threadId, turn_id: 'turn-one', role: 'tool', kind: 'tool_result', text: 'Read 180 lines', data: { call_id: 'read', ok: true }, ts: now },
        { id: 'call-test', thread_id: message.threadId, turn_id: 'turn-one', role: 'assistant', kind: 'tool_call', text: 'exec_command', data: { call_id: 'test', input: { command: 'npm test' } }, ts: now },
        { id: 'result-test', thread_id: message.threadId, turn_id: 'turn-one', role: 'tool', kind: 'tool_result', text: '74 tests passed', data: { call_id: 'test', ok: true }, ts: now },
        { id: 'file-changed', thread_id: message.threadId, turn_id: 'turn-one', role: 'assistant', kind: 'file_changed', text: 'Modified src/accounts.tsx', data: {}, ts: now },
      ];
      snapshot.details.events.splice(1, 0, ...toolEvents);
      if (message.threadId === 'session-three') {
        snapshot.details.events = snapshot.details.events.filter((event) => !['assistant-one', 'result-test'].includes(event.id));
      }
    }
    if (message.type === "loadDiff" && snapshot.details) {
      snapshot.details.diffState = "ready";
      snapshot.details.diff = { repos: [{ repo_id: "repo-one", repo_name: "Perpetual", remote_url: null, branch: "am/thread-preview", base_ref: "dev", head_ref: "HEAD", worktree_path: "C:/Development/Perpetual", files: [{ path: "src/accounts.tsx", additions: 2, deletions: 1 }], patch: "diff --git a/src/accounts.tsx b/src/accounts.tsx\n--- a/src/accounts.tsx\n+++ b/src/accounts.tsx\n@@ -1,2 +1,3 @@\n export function accounts() {\n-  return profiles;\n+  const identities = uniqueAccountChoices(profiles);\n+  return identities;\n" }] };
    }
    if (message.type === "loadDiff" && snapshot.details?.diff && params.has("managed-review")) snapshot.details.diff.repos[0].worktree_path = "C:/Development/.am/worktrees/preview";
    if (message.type === "loadDiff" && snapshot.details?.diff && params.has("empty-review")) { snapshot.details.diff.repos[0].files = []; snapshot.details.diff.repos[0].patch = ""; }
    if (message.type === "activateProviderAccount") { const selected = snapshot.providerAccounts.find((a) => a.id === message.accountId)!; snapshot.providerAccounts.forEach((a) => { if (a.agent === selected.agent) a.active = a.id === selected.id && a.availability !== "limited"; }); }
    if (message.type === "updateProviderAccount") snapshot.providerAccounts = snapshot.providerAccounts.map((a) => a.id === message.accountId ? { ...a, ...message.patch } : a);
    if (message.type === "renameThread") snapshot.threads = snapshot.threads.map((thread) => thread.id === message.threadId ? { ...thread, title: message.title } : thread);
    if (message.type === "reorderThreads") snapshot.threads.sort((a, b) => message.orderedIds.indexOf(a.id) - message.orderedIds.indexOf(b.id));
    if (message.type === "deleteProviderAccount") snapshot.providerAccounts = snapshot.providerAccounts.filter((a) => a.id !== message.accountId);
    if (message.type === "reorderProviderAccounts") snapshot.providerAccounts.sort((a, b) => message.orderedIds.indexOf(a.id) - message.orderedIds.indexOf(b.id));
    if (message.type === "addProviderAccount") snapshot.providerAccounts.push({ ...message.account, installed: true, authenticated: false, active: false, availability: "unknown", reset_at: null, detail: null });
    if (message.type === "setLimitPolicy") snapshot.limitPolicy = { ...message.policy, accounts: snapshot.providerAccounts };
    if (message.type === "submit") emit({ type: "submitFailed", threadId: message.threadId, clientMessageId: message.clientMessageId, text: message.message, message: "Preview mode: no provider calls are made. Your draft has been restored." });
    refresh();
    if (message.requestId) emit({ type: "operationResult", requestId: message.requestId, error: params.has("fail") && message.type === "setProviderAccountToken" ? "Could not save token. Please try again." : null });
  }, message.type === "githubList" ? Number(params.get("github-delay") ?? 180) : 180);
} });
await import("./src/main");
if (params.has("refresh")) window.setInterval(refresh, 1_000);
