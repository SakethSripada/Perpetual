import { useEffect, useMemo, useRef, useState } from "react";
import type {
  AgentKind,
  AgentStatus,
  AgentThread,
  ExecutionBackend,
  ExtensionMessage,
  GithubRepository,
  LimitPolicy,
  PermissionPolicy,
  SandboxPolicy,
  WorkbenchSnapshot,
} from "./types";

type VsCodeApi = {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
};

declare global {
  interface Window {
    acquireVsCodeApi?: () => VsCodeApi;
  }
}

const vscode =
  window.acquireVsCodeApi?.() ??
  ({
    postMessage: (message: unknown) => console.info("webview message", message),
    getState: () => ({}),
    setState: () => undefined,
  } satisfies VsCodeApi);

export default function App() {
  const [snapshot, setSnapshot] = useState<WorkbenchSnapshot | null>(null);
  const [message, setMessage] = useState("");
  const [agent, setAgent] = useState<AgentKind>("claude_code");
  const [permission, setPermission] = useState<PermissionPolicy>("workspace_write");
  const [backend, setBackend] = useState<ExecutionBackend>("host");
  const [model, setModel] = useState("");
  const [reasoning, setReasoning] = useState("medium");
  const [repoIds, setRepoIds] = useState<string[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubRepos, setGithubRepos] = useState<GithubRepository[]>([]);
  const [githubLoading, setGithubLoading] = useState(false);
  const transcriptRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const onMessage = (event: MessageEvent<ExtensionMessage>) => {
      const incoming = event.data;
      if (incoming.type === "snapshot") {
        setSnapshot(incoming.snapshot);
        if (incoming.snapshot.error) setNotice(incoming.snapshot.error);
        return;
      }
      if (incoming.type === "githubRepos") {
        setGithubRepos(incoming.repos);
        setGithubLoading(false);
        setGithubOpen(true);
        return;
      }
      if (incoming.type === "notice" || incoming.type === "error") {
        setNotice(incoming.message);
        return;
      }
      if (incoming.type === "sandboxLoginPrompt") {
        setNotice(`Sandbox code: ${incoming.prompt.code}`);
      }
    };
    window.addEventListener("message", onMessage);
    vscode.postMessage({ type: "ready" });
    return () => window.removeEventListener("message", onMessage);
  }, []);

  const selectedThread = useMemo(
    () => snapshot?.threads.find((thread) => thread.id === snapshot.selectedThreadId) ?? null,
    [snapshot]
  );
  const selectedAgentStatus = useMemo(
    () => snapshot?.agents.find((status) => status.kind === agent) ?? null,
    [snapshot, agent]
  );

  useEffect(() => {
    if (!snapshot) return;
    const nextAgent = selectedThread?.preferred_agent ?? selectedThread?.active_agent ?? snapshot.defaults.agent;
    const defaults = runDefaults(snapshot, nextAgent);
    setAgent(nextAgent);
    setPermission(selectedThread?.permission ?? snapshot.defaults.permission);
    setBackend(sanitizeBackend(nextAgent, selectedThread?.execution_backend ?? snapshot.defaults.execution_backend));
    setModel(selectedThread?.model ?? snapshot.defaults.model ?? defaults.model ?? "");
    setReasoning(selectedThread?.reasoning ?? snapshot.defaults.reasoning ?? defaults.reasoning ?? "medium");
    if (selectedThread && snapshot.details?.repos.length) {
      setRepoIds(snapshot.details.repos.map((repo) => repo.repo_id));
    } else if (!selectedThread) {
      setRepoIds(snapshot.repos.map((repo) => repo.id));
    }
  }, [snapshot?.selectedThreadId, snapshot?.repos.length]);

  useEffect(() => {
    transcriptRef.current?.scrollTo({
      top: transcriptRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, [snapshot?.details?.events.length, selectedThread?.id]);

  const isRunning = selectedThread?.status === "running";
  const canSend = !!message.trim() && !!snapshot?.trusted;
  const details = snapshot?.details;

  const send = () => {
    if (!canSend) return;
    const text = message.trim();
    setMessage("");
    vscode.postMessage({
      type: "submit",
      message: text,
      threadId: selectedThread?.id ?? null,
      repoIds,
      agent,
      permission,
      executionBackend: sanitizeBackend(agent, backend),
      model: model.trim() || null,
      reasoning: reasoning.trim() || null,
    });
  };

  const pickAgent = (nextAgent: AgentKind) => {
    setAgent(nextAgent);
    if (nextAgent !== "codex") setBackend("host");
    if (!snapshot) return;
    const defaults = runDefaults(snapshot, nextAgent);
    if (!model.trim()) setModel(defaults.model ?? "");
    if (!reasoning.trim()) setReasoning(defaults.reasoning ?? "medium");
  };

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <img src={iconUri()} alt="" />
          <div>
            <strong>AgentManager</strong>
            <span>{selectedThread?.title ?? "Workbench"}</span>
          </div>
        </div>
        <div className="top-actions">
          <button title="New session" onClick={() => vscode.postMessage({ type: "newSession" })}>
            +
          </button>
          <button title="Open wider panel" onClick={() => vscode.postMessage({ type: "openPanel" })}>
            Panel
          </button>
          <button title="Refresh" onClick={() => vscode.postMessage({ type: "refresh" })}>
            Refresh
          </button>
          <button title="Settings" onClick={() => setSettingsOpen(true)}>
            Settings
          </button>
        </div>
      </header>

      {notice && (
        <div className="notice" role="status">
          <span>{notice}</span>
          <button onClick={() => setNotice(null)} title="Dismiss">
            x
          </button>
        </div>
      )}

      <section className="status-strip">
        <AgentPill status={snapshot?.agents.find((status) => status.kind === "claude_code") ?? null} />
        <AgentPill status={snapshot?.agents.find((status) => status.kind === "codex") ?? null} />
        <span className={snapshot?.sandboxRuntime?.installed ? "runtime ok" : "runtime"}>
          Docker {snapshot?.sandboxRuntime?.installed ? "ready" : "off"}
        </span>
        {selectedThread?.handoff_state && (
          <span className="handoff">{humanize(selectedThread.handoff_state)}</span>
        )}
      </section>

      <div className="workspace-grid">
        <aside className="sessions">
          <button
            className={!selectedThread ? "session active" : "session"}
            onClick={() => vscode.postMessage({ type: "newSession" })}
          >
            <span>New session</span>
            <small>{snapshot?.repos.length ?? 0} repos</small>
          </button>
          {snapshot?.threads.map((thread) => (
            <button
              key={thread.id}
              className={thread.id === snapshot.selectedThreadId ? "session active" : "session"}
              onClick={() => vscode.postMessage({ type: "selectThread", threadId: thread.id })}
            >
              <span>{thread.title}</span>
              <small>
                {labelAgent(thread.active_agent ?? thread.preferred_agent)} / {humanize(thread.status)}
              </small>
            </button>
          ))}
        </aside>

        <section className="conversation">
          <div className="thread-meta">
            <div>
              <strong>{selectedThread?.title ?? "Untitled"}</strong>
              <span>{selectedThread ? humanize(selectedThread.status) : "Draft"}</span>
            </div>
            {selectedThread && (
              <div className="thread-actions">
                {isRunning && (
                  <button onClick={() => vscode.postMessage({ type: "stopThread", threadId: selectedThread.id })}>
                    Stop
                  </button>
                )}
                <button
                  onClick={() =>
                    vscode.postMessage({ type: "deleteThread", threadId: selectedThread.id, force: isRunning })
                  }
                >
                  Delete
                </button>
              </div>
            )}
          </div>

          <div className="transcript" ref={transcriptRef}>
            {!selectedThread && <EmptyState trusted={snapshot?.trusted ?? true} />}
            {selectedThread &&
              details?.events.map((event) => (
                <article key={event.id} className={`message ${messageClass(event.role)}`}>
                  <div className="message-meta">
                    <span>{event.role || event.kind}</span>
                    <time>{formatTime(event.ts)}</time>
                  </div>
                  <div className="message-body">{event.text || humanize(event.kind)}</div>
                </article>
              ))}
            {selectedThread && details && details.events.length === 0 && (
              <EmptyState trusted={snapshot?.trusted ?? true} compact />
            )}
          </div>

          {selectedThread && details && (
            <RunDetails
              thread={selectedThread}
              details={details}
              onOpenPath={(target) => vscode.postMessage({ type: "openPath", path: target })}
              onDeleteQueued={(id) => vscode.postMessage({ type: "deleteQueuedTurn", id })}
              onMoveQueued={(orderedIds) =>
                vscode.postMessage({ type: "reorderQueuedTurns", threadId: selectedThread.id, orderedIds })
              }
            />
          )}
        </section>
      </div>

      <Composer
        snapshot={snapshot}
        selectedThread={selectedThread}
        selectedAgentStatus={selectedAgentStatus}
        message={message}
        setMessage={setMessage}
        agent={agent}
        setAgent={pickAgent}
        permission={permission}
        setPermission={setPermission}
        backend={backend}
        setBackend={setBackend}
        model={model}
        setModel={setModel}
        reasoning={reasoning}
        setReasoning={setReasoning}
        repoIds={repoIds}
        setRepoIds={setRepoIds}
        isRunning={isRunning}
        canSend={canSend}
        onSend={send}
        onGithub={() => {
          setGithubLoading(true);
          vscode.postMessage({ type: "githubList" });
        }}
        onWorkspaceRepos={() => vscode.postMessage({ type: "connectWorkspaceRepos" })}
        onSandboxLogin={(codex) => vscode.postMessage({ type: "sandboxLogin", codex })}
      />

      {settingsOpen && snapshot && (
        <SettingsSheet
          snapshot={snapshot}
          onClose={() => setSettingsOpen(false)}
          onApply={(limitPolicy, sandboxPolicy) => {
            vscode.postMessage({ type: "setLimitPolicy", policy: limitPolicy });
            vscode.postMessage({ type: "setSandboxPolicy", policy: sandboxPolicy });
            setSettingsOpen(false);
          }}
          onOpenSettings={() => vscode.postMessage({ type: "openSettings" })}
        />
      )}

      {githubOpen && (
        <GithubSheet
          loading={githubLoading}
          repos={githubRepos}
          onClose={() => setGithubOpen(false)}
          onConnect={(repo) => {
            setGithubOpen(false);
            vscode.postMessage({ type: "connectGithubRepo", repo });
          }}
        />
      )}
    </main>
  );
}

function Composer(props: {
  snapshot: WorkbenchSnapshot | null;
  selectedThread: AgentThread | null;
  selectedAgentStatus: AgentStatus | null;
  message: string;
  setMessage(value: string): void;
  agent: AgentKind;
  setAgent(value: AgentKind): void;
  permission: PermissionPolicy;
  setPermission(value: PermissionPolicy): void;
  backend: ExecutionBackend;
  setBackend(value: ExecutionBackend): void;
  model: string;
  setModel(value: string): void;
  reasoning: string;
  setReasoning(value: string): void;
  repoIds: string[];
  setRepoIds(value: string[]): void;
  isRunning: boolean;
  canSend: boolean;
  onSend(): void;
  onGithub(): void;
  onWorkspaceRepos(): void;
  onSandboxLogin(codex: boolean): void;
}) {
  const dockerAllowed = props.agent === "codex";
  const sandbox = props.snapshot?.sandboxRuntime;
  return (
    <footer className="composer">
      <textarea
        value={props.message}
        placeholder={props.isRunning ? "Queue a follow-up" : "Do anything"}
        onChange={(event) => props.setMessage(event.target.value)}
        onKeyDown={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
            event.preventDefault();
            props.onSend();
          }
        }}
      />

      <div className="repo-row">
        <div className="repo-picks">
          {props.snapshot?.repos.map((repo) => {
            const checked = props.repoIds.includes(repo.id);
            return (
              <label key={repo.id} className={checked ? "repo-chip selected" : "repo-chip"}>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={!!props.selectedThread}
                  onChange={(event) => {
                    const next = event.target.checked
                      ? [...props.repoIds, repo.id]
                      : props.repoIds.filter((id) => id !== repo.id);
                    props.setRepoIds(next);
                  }}
                />
                {repo.name}
              </label>
            );
          })}
        </div>
        <button onClick={props.onWorkspaceRepos}>Local</button>
        <button onClick={props.onGithub}>GitHub</button>
      </div>

      <div className="control-grid">
        <div className="segmented">
          <button
            className={props.agent === "claude_code" ? "selected" : ""}
            onClick={() => props.setAgent("claude_code")}
          >
            Claude
          </button>
          <button
            className={props.agent === "codex" ? "selected" : ""}
            onClick={() => props.setAgent("codex")}
          >
            Codex
          </button>
        </div>

        <select value={props.permission} onChange={(event) => props.setPermission(event.target.value as PermissionPolicy)}>
          <option value="read_only">Read</option>
          <option value="workspace_write">Write</option>
          <option value="autonomous">Autonomous</option>
        </select>

        <div className="segmented">
          <button
            className={props.backend === "host" ? "selected" : ""}
            onClick={() => props.setBackend("host")}
          >
            Host
          </button>
          <button
            className={props.backend === "docker_sandbox" ? "selected" : ""}
            disabled={!dockerAllowed}
            onClick={() => props.setBackend("docker_sandbox")}
            title={dockerAllowed ? "Use Codex Docker Sandbox" : "Claude Code uses Host in this release"}
          >
            Docker
          </button>
        </div>

        <select value={props.reasoning} onChange={(event) => props.setReasoning(event.target.value)}>
          <option value="">Default</option>
          <option value="low">Low</option>
          <option value="medium">Medium</option>
          <option value="high">High</option>
        </select>

        <input
          value={props.model}
          placeholder="Model"
          onChange={(event) => props.setModel(event.target.value)}
        />

        <button className="send" disabled={!props.canSend} onClick={props.onSend}>
          {props.isRunning ? "Queue" : "Send"}
        </button>
      </div>

      <div className="composer-status">
        <span>{agentStateText(props.selectedAgentStatus)}</span>
        {props.backend === "docker_sandbox" && sandbox && !sandbox.authenticated && (
          <button onClick={() => props.onSandboxLogin(false)}>Sign in sbx</button>
        )}
        {props.backend === "docker_sandbox" && sandbox && !sandbox.codex_authenticated && (
          <button onClick={() => props.onSandboxLogin(true)}>Sign in Codex sandbox</button>
        )}
        {!dockerAllowed && <span>Claude Code is Host-only in v1.</span>}
      </div>
    </footer>
  );
}

function RunDetails(props: {
  thread: AgentThread;
  details: NonNullable<WorkbenchSnapshot["details"]>;
  onOpenPath(path: string): void;
  onDeleteQueued(id: string): void;
  onMoveQueued(orderedIds: string[]): void;
}) {
  const queuedIds = props.details.queued.map((turn) => turn.id);
  return (
    <section className="run-details">
      {props.details.repos.length > 0 && (
        <div className="detail-band">
          {props.details.repos.map((repo) => (
            <div key={repo.repo_id} className="repo-detail">
              <span>{repo.repo_name}</span>
              <small>{repo.branch ?? repo.workspace_backend}</small>
              {repo.worktree_path && <button onClick={() => props.onOpenPath(repo.worktree_path!)}>Open</button>}
            </div>
          ))}
        </div>
      )}
      {props.details.queued.length > 0 && (
        <div className="queue-list">
          <strong>Queued</strong>
          {props.details.queued.map((turn, index) => (
            <div className="queue-item" key={turn.id}>
              <span>{turn.message}</span>
              <button
                disabled={index === 0}
                onClick={() => props.onMoveQueued(move(queuedIds, index, index - 1))}
              >
                Up
              </button>
              <button
                disabled={index === queuedIds.length - 1}
                onClick={() => props.onMoveQueued(move(queuedIds, index, index + 1))}
              >
                Down
              </button>
              <button onClick={() => props.onDeleteQueued(turn.id)}>Remove</button>
            </div>
          ))}
        </div>
      )}
      {props.details.diff?.repos.some((repo) => repo.files.length > 0) && (
        <div className="diff-strip">
          {props.details.diff.repos.map((repo) =>
            repo.files.map((file) => (
              <span key={`${repo.repo_id}:${file.path}`}>
                {file.path} +{file.additions} -{file.deletions}
              </span>
            ))
          )}
        </div>
      )}
    </section>
  );
}

function SettingsSheet(props: {
  snapshot: WorkbenchSnapshot;
  onClose(): void;
  onApply(limitPolicy: LimitPolicy, sandboxPolicy: SandboxPolicy): void;
  onOpenSettings(): void;
}) {
  const [limit, setLimit] = useState<LimitPolicy>(() => props.snapshot.limitPolicy ?? defaultLimitPolicy());
  const [sandbox, setSandbox] = useState<SandboxPolicy>(() => props.snapshot.sandboxPolicy ?? defaultSandboxPolicy());
  const claudeFirst = limit.agent_priority[0] !== "codex";

  return (
    <div className="sheet-backdrop">
      <section className="sheet">
        <header>
          <strong>Settings</strong>
          <button onClick={props.onClose}>x</button>
        </header>
        <div className="settings-grid">
          <label>
            <input
              type="checkbox"
              checked={limit.auto_switch}
              onChange={(event) => setLimit({ ...limit, auto_switch: event.target.checked })}
            />
            Auto switch on limits
          </label>
          <label>
            <input
              type="checkbox"
              checked={limit.switch_back}
              onChange={(event) => setLimit({ ...limit, switch_back: event.target.checked })}
            />
            Switch back on recovery
          </label>
          <label>
            <input
              type="checkbox"
              checked={limit.resume_with_earliest}
              onChange={(event) => setLimit({ ...limit, resume_with_earliest: event.target.checked })}
            />
            Resume with earliest agent
          </label>
          <label>
            Retry seconds
            <input
              type="number"
              min={0}
              value={limit.unknown_reset_retry_secs}
              onChange={(event) =>
                setLimit({ ...limit, unknown_reset_retry_secs: Number(event.target.value) })
              }
            />
          </label>
          <div className="field-row">
            <span>Fallback</span>
            <div className="segmented">
              <button
                className={claudeFirst ? "selected" : ""}
                onClick={() => setLimit({ ...limit, agent_priority: ["claude_code", "codex"] })}
              >
                Claude first
              </button>
              <button
                className={!claudeFirst ? "selected" : ""}
                onClick={() => setLimit({ ...limit, agent_priority: ["codex", "claude_code"] })}
              >
                Codex first
              </button>
            </div>
          </div>
          <label>
            Default runtime
            <select
              value={sandbox.default_backend}
              onChange={(event) =>
                setSandbox({ ...sandbox, default_backend: event.target.value as ExecutionBackend })
              }
            >
              <option value="host">Host</option>
              <option value="docker_sandbox">Docker Sandbox</option>
            </select>
          </label>
          <label>
            Max sandboxes
            <input
              type="number"
              min={1}
              max={8}
              value={sandbox.max_concurrent_sandboxes}
              onChange={(event) =>
                setSandbox({ ...sandbox, max_concurrent_sandboxes: Number(event.target.value) })
              }
            />
          </label>
          <label>
            CPUs
            <input
              type="number"
              min={1}
              max={16}
              value={sandbox.cpus}
              onChange={(event) => setSandbox({ ...sandbox, cpus: Number(event.target.value) })}
            />
          </label>
          <label>
            Memory
            <input value={sandbox.memory} onChange={(event) => setSandbox({ ...sandbox, memory: event.target.value })} />
          </label>
          <label>
            Network
            <select
              value={sandbox.network_preset}
              onChange={(event) => setSandbox({ ...sandbox, network_preset: event.target.value })}
            >
              <option value="balanced">Balanced</option>
              <option value="open">Open</option>
              <option value="locked_down">Locked down</option>
            </select>
          </label>
        </div>
        <footer>
          <button onClick={props.onOpenSettings}>VS Code settings</button>
          <button className="primary" onClick={() => props.onApply(limit, sandbox)}>
            Apply
          </button>
        </footer>
      </section>
    </div>
  );
}

function GithubSheet(props: {
  loading: boolean;
  repos: GithubRepository[];
  onClose(): void;
  onConnect(repo: GithubRepository): void;
}) {
  return (
    <div className="sheet-backdrop">
      <section className="sheet repo-sheet">
        <header>
          <strong>GitHub</strong>
          <button onClick={props.onClose}>x</button>
        </header>
        <div className="github-list">
          {props.loading && <div className="empty">Loading repositories</div>}
          {!props.loading &&
            props.repos.map((repo) => (
              <button key={repo.id} className="github-row" onClick={() => props.onConnect(repo)}>
                <span>{repo.full_name}</span>
                <small>{repo.private ? "Private" : "Public"} / {repo.default_branch}</small>
              </button>
            ))}
        </div>
      </section>
    </div>
  );
}

function AgentPill({ status }: { status: AgentStatus | null }) {
  const kind = status?.kind ?? "claude_code";
  const ready = !!status?.installed && !!status?.authenticated && status.availability !== "limited";
  return (
    <span className={ready ? "agent-pill ready" : "agent-pill"}>
      {labelAgent(kind)} {status ? humanize(status.availability) : "unknown"}
    </span>
  );
}

function EmptyState({ trusted, compact = false }: { trusted: boolean; compact?: boolean }) {
  return (
    <div className={compact ? "empty compact" : "empty"}>
      <img src={iconUri()} alt="" />
      <strong>{trusted ? "Ready" : "Restricted Mode"}</strong>
      <span>{trusted ? "Start from the composer." : "Trust this workspace to run agents."}</span>
    </div>
  );
}

function runDefaults(snapshot: WorkbenchSnapshot, agent: AgentKind) {
  return snapshot.runDefaults.find((item) => item.kind === agent) ?? { model: null, reasoning: null };
}

function iconUri(): string {
  return document.getElementById("root")?.getAttribute("data-icon") ?? "";
}

function sanitizeBackend(agent: AgentKind, backend: ExecutionBackend): ExecutionBackend {
  return backend === "docker_sandbox" && agent !== "codex" ? "host" : backend;
}

function labelAgent(agent: AgentKind | null | undefined): string {
  switch (agent) {
    case "claude_code":
      return "Claude";
    case "codex":
      return "Codex";
    case "cursor":
      return "Cursor";
    case "gemini":
      return "Gemini";
    case "open_code":
      return "OpenCode";
    default:
      return "Agent";
  }
}

function agentStateText(status: AgentStatus | null): string {
  if (!status) return "Agent status unknown";
  if (!status.installed) return `${labelAgent(status.kind)} CLI not found`;
  if (!status.authenticated) return `${labelAgent(status.kind)} auth required`;
  if (status.availability === "limited") return `${labelAgent(status.kind)} limited`;
  return `${labelAgent(status.kind)} ready`;
}

function humanize(value: string): string {
  return value
    .split("_")
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

function messageClass(role: string): string {
  if (role === "user") return "user";
  if (role === "assistant") return "assistant";
  return "system";
}

function formatTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function move(ids: string[], from: number, to: number): string[] {
  const next = [...ids];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

function defaultLimitPolicy(): LimitPolicy {
  return {
    auto_switch: true,
    switch_back: true,
    agent_priority: ["claude_code", "codex"],
    resume_with_earliest: true,
    unknown_reset_retry_secs: 600,
    keep_awake: true,
  };
}

function defaultSandboxPolicy(): SandboxPolicy {
  return {
    default_backend: "host",
    max_concurrent_sandboxes: 2,
    cpus: 2,
    memory: "4g",
    network_preset: "balanced",
    run_timeout_secs: 0,
    idle_timeout_secs: 0,
    stop_grace_secs: 10,
  };
}
