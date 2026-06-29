import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type {
  AgentKind,
  AgentThread,
  ApprovalDecision,
  ApprovalRequest,
  AvailabilityState,
  ExecutionBackend,
  ExtensionMessage,
  GithubRepository,
  LimitPolicy,
  PermissionPolicy,
  SandboxPolicy,
  WorkbenchSnapshot,
} from "./types";
import { BrandMark, Icon } from "./icons";
import { Markdown } from "./markdown";

type PendingMessage = { id: string; text: string };

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
  const [historyOpen, setHistoryOpen] = useState(false);
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubRepos, setGithubRepos] = useState<GithubRepository[]>([]);
  const [githubLoading, setGithubLoading] = useState(false);
  // Optimistically-rendered user messages: shown the instant the user sends, then
  // dropped once the real event for them arrives in a snapshot.
  const [pending, setPending] = useState<PendingMessage[]>([]);
  // Optimistic thread navigation: reflect the clicked thread instantly instead of
  // waiting for the round-trip. `undefined` means "no navigation in flight".
  const [navThreadId, setNavThreadId] = useState<string | null | undefined>(undefined);
  const transcriptRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const onMessage = (event: MessageEvent<ExtensionMessage>) => {
      const incoming = event.data;
      if (incoming.type === "snapshot") {
        setSnapshot(incoming.snapshot);
        if (incoming.snapshot.error) setNotice(incoming.snapshot.error);
        // Clear the optimistic navigation once the daemon agrees on the selection.
        setNavThreadId((nav) => (nav === undefined || nav === incoming.snapshot.selectedThreadId ? undefined : nav));
        // Drop optimistic bubbles once the real message lands — either as a user
        // event (sent immediately) or as a queued turn (sent while running).
        const events = incoming.snapshot.details?.events ?? [];
        const queued = incoming.snapshot.details?.queued ?? [];
        setPending((prev) =>
          prev.filter(
            (item) =>
              !events.some((e) => e.role === "user" && (e.text ?? "").trim() === item.text) &&
              !queued.some((q) => q.message.trim() === item.text)
          )
        );
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

  const effectiveSelectedId = navThreadId !== undefined ? navThreadId : snapshot?.selectedThreadId ?? null;
  const selectedThread = useMemo(
    () => snapshot?.threads.find((thread) => thread.id === effectiveSelectedId) ?? null,
    [snapshot, effectiveSelectedId]
  );
  // Details belong to the daemon's current selection; while navigating to a
  // different thread they're stale, so we show a loading state instead.
  const navigating = navThreadId !== undefined && navThreadId !== (snapshot?.selectedThreadId ?? null);
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
  }, [effectiveSelectedId, snapshot?.repos.length]);

  useEffect(() => {
    transcriptRef.current?.scrollTo({
      top: transcriptRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, [snapshot?.details?.events.length, selectedThread?.id, pending.length]);

  // Surface agent availability changes as a notice instead of an always-on badge.
  const availabilityRef = useRef<Record<string, AvailabilityState>>({});
  useEffect(() => {
    if (!snapshot) return;
    for (const status of snapshot.agents) {
      const previous = availabilityRef.current[status.kind];
      availabilityRef.current[status.kind] = status.availability;
      if (!previous || previous === status.availability) continue;
      if (status.availability === "limited") {
        const until = status.reset_at ? ` until ${formatTime(status.reset_at)}` : "";
        setNotice(`${labelAgent(status.kind)} is rate limited${until}.`);
      } else if (previous === "limited" && status.availability === "available") {
        setNotice(`${labelAgent(status.kind)} is available again.`);
      }
    }
  }, [snapshot]);

  const isRunning = selectedThread?.status === "running";
  const canSend = !!message.trim() && !!snapshot?.trusted;
  const details = navigating ? undefined : snapshot?.details;

  const send = () => {
    if (!canSend) return;
    const text = message.trim();
    setMessage("");
    setPending((prev) => [...prev, { id: `pending-${Date.now()}-${prev.length}`, text }]);
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

  const newSession = () => {
    setHistoryOpen(false);
    setPending([]);
    setNavThreadId(undefined);
    vscode.postMessage({ type: "newSession" });
  };

  const selectThread = (id: string) => {
    setHistoryOpen(false);
    setPending([]);
    setNavThreadId(id);
    vscode.postMessage({ type: "selectThread", threadId: id });
  };

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">
            <BrandMark size={18} />
          </span>
          <div className="brand-text">
            <strong>{selectedThread?.title ?? "AgentManager"}</strong>
            <span>{selectedThread ? humanize(selectedThread.status) : "Workbench"}</span>
          </div>
        </div>
        <div className="top-actions">
          {selectedThread && isRunning && (
            <IconButton
              title="Stop"
              onClick={() => vscode.postMessage({ type: "stopThread", threadId: selectedThread.id })}
            >
              <Icon name="stop" />
            </IconButton>
          )}
          {selectedThread && (
            <IconButton
              title="Delete session"
              onClick={() =>
                vscode.postMessage({ type: "deleteThread", threadId: selectedThread.id, force: isRunning })
              }
            >
              <Icon name="trash" />
            </IconButton>
          )}
          <IconButton title="New session" onClick={newSession}>
            <Icon name="plus" />
          </IconButton>
          <HistoryMenu
            open={historyOpen}
            setOpen={setHistoryOpen}
            snapshot={snapshot}
            selectedThread={selectedThread}
            onNew={newSession}
            onSelect={selectThread}
          />
          <IconButton title="Open in editor" onClick={() => vscode.postMessage({ type: "openPanel" })}>
            <Icon name="window" />
          </IconButton>
          <IconButton title="Refresh" onClick={() => vscode.postMessage({ type: "refresh" })}>
            <Icon name="refresh" />
          </IconButton>
          <IconButton title="Settings" onClick={() => setSettingsOpen(true)}>
            <Icon name="settings" />
          </IconButton>
        </div>
      </header>

      {notice && (
        <div className="notice" role="status">
          <span>{notice}</span>
          <IconButton title="Dismiss" onClick={() => setNotice(null)}>
            <Icon name="close" />
          </IconButton>
        </div>
      )}

      <section className="conversation">
        <div className="transcript" ref={transcriptRef}>
          {navigating && <div className="thinking">Loading…</div>}
          {!navigating && !selectedThread && pending.length === 0 && (
            <EmptyState trusted={snapshot?.trusted ?? true} />
          )}
          {selectedThread &&
            details?.events.map((event) => (
              <article key={event.id} className={`msg ${messageClass(event.role)}`}>
                <div className="msg-head">
                  <span className="msg-role">{roleLabel(event.role, event.kind, agent)}</span>
                  <time>{formatTime(event.ts)}</time>
                </div>
                <div className="msg-body">
                  {event.text ? <Markdown text={event.text} /> : humanize(event.kind)}
                </div>
              </article>
            ))}
          {pending.map((item) => (
            <article key={item.id} className="msg user pending">
              <div className="msg-head">
                <span className="msg-role">You</span>
                <Icon name="clock" className="pending-spinner" />
              </div>
              <div className="msg-body">{item.text}</div>
            </article>
          ))}
          {details?.approvals.map((approval) => (
            <ApprovalCard
              key={approval.id}
              approval={approval}
              onResolve={(decision) =>
                vscode.postMessage({ type: "resolveApproval", id: approval.id, decision })
              }
            />
          ))}
          {!navigating && (isRunning || pending.length > 0) && !details?.approvals.length && (
            <div className="thinking">Working…</div>
          )}
          {!navigating &&
            selectedThread &&
            details &&
            details.events.length === 0 &&
            pending.length === 0 &&
            !isRunning && <EmptyState trusted={snapshot?.trusted ?? true} compact />}
        </div>

        {selectedThread && details && (
          <RunDetails
            details={details}
            onOpenPath={(target) => vscode.postMessage({ type: "openPath", path: target })}
            onDeleteQueued={(id) => vscode.postMessage({ type: "deleteQueuedTurn", id })}
            onMoveQueued={(orderedIds) =>
              vscode.postMessage({ type: "reorderQueuedTurns", threadId: selectedThread.id, orderedIds })
            }
          />
        )}
      </section>

      <Composer
        snapshot={snapshot}
        selectedThread={selectedThread}
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

function IconButton(props: { title: string; onClick(): void; disabled?: boolean; children: ReactNode }) {
  return (
    <button className="icon-btn" type="button" title={props.title} aria-label={props.title} disabled={props.disabled} onClick={props.onClick}>
      {props.children}
    </button>
  );
}

/** Generic viewport-anchored popover used by toolbar menus. */
function Popover(props: {
  trigger: (args: { open: boolean; toggle(): void; ref: (el: HTMLElement | null) => void }) => ReactNode;
  open: boolean;
  setOpen(open: boolean): void;
  align?: "left" | "right";
  children: ReactNode;
}) {
  const triggerRef = useRef<HTMLElement | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuStyle, setMenuStyle] = useState<CSSProperties>({});
  const align = props.align ?? "left";

  useLayoutEffect(() => {
    if (!props.open) return;
    const reposition = () => {
      const trigger = triggerRef.current;
      if (!trigger) return;
      const rect = trigger.getBoundingClientRect();
      const margin = 6;
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const spaceBelow = vh - rect.bottom - margin;
      const spaceAbove = rect.top - margin;
      const openUp = spaceBelow < 220 && spaceAbove > spaceBelow;
      const maxHeight = Math.max(120, Math.min(360, openUp ? spaceAbove : spaceBelow));
      const maxWidth = vw - margin * 2;
      const menuWidth = Math.min(menuRef.current?.offsetWidth ?? rect.width, maxWidth);
      // Anchor by preferred edge, then clamp fully inside the viewport so menus never clip.
      const preferredLeft = align === "right" ? rect.right - menuWidth : rect.left;
      const left = Math.max(margin, Math.min(preferredLeft, vw - menuWidth - margin));
      const next: CSSProperties = {
        position: "fixed",
        maxHeight,
        maxWidth,
        minWidth: Math.min(rect.width, maxWidth),
        left,
      };
      if (openUp) next.bottom = vh - rect.top + margin;
      else next.top = rect.bottom + margin;
      setMenuStyle(next);
    };
    reposition();
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    return () => {
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [props.open, align]);

  useEffect(() => {
    if (!props.open) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!menuRef.current?.contains(target) && !triggerRef.current?.contains(target)) {
        props.setOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") props.setOpen(false);
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [props.open]);

  return (
    <>
      {props.trigger({
        open: props.open,
        toggle: () => props.setOpen(!props.open),
        ref: (el) => (triggerRef.current = el),
      })}
      {props.open && (
        <div ref={menuRef} className="popover" style={menuStyle}>
          {props.children}
        </div>
      )}
    </>
  );
}

function Dropdown(props: {
  value: string;
  options: { value: string; label: string }[];
  onChange(value: string): void;
  icon?: ReactNode;
  ariaLabel?: string;
  title?: string;
}) {
  const [open, setOpen] = useState(false);
  const selected = props.options.find((option) => option.value === props.value);
  return (
    <Popover
      open={open}
      setOpen={setOpen}
      trigger={({ toggle, ref }) => (
        <button
          ref={ref as (el: HTMLButtonElement | null) => void}
          type="button"
          className="chip-btn"
          title={props.title}
          aria-label={props.ariaLabel}
          aria-haspopup="listbox"
          aria-expanded={open}
          onClick={toggle}
        >
          {props.icon}
          <span className="chip-label">{selected?.label ?? props.value}</span>
          <Icon name="caret" className="chip-caret" />
        </button>
      )}
    >
      <div className="menu" role="listbox">
        {props.options.map((option) => (
          <button
            key={option.value}
            type="button"
            role="option"
            aria-selected={option.value === props.value}
            className={option.value === props.value ? "menu-item selected" : "menu-item"}
            onClick={() => {
              props.onChange(option.value);
              setOpen(false);
            }}
          >
            <span>{option.label}</span>
            {option.value === props.value && <Icon name="check" />}
          </button>
        ))}
      </div>
    </Popover>
  );
}

function HistoryMenu(props: {
  open: boolean;
  setOpen(open: boolean): void;
  snapshot: WorkbenchSnapshot | null;
  selectedThread: AgentThread | null;
  onNew(): void;
  onSelect(id: string): void;
}) {
  const threads = props.snapshot?.threads ?? [];
  return (
    <Popover
      open={props.open}
      setOpen={props.setOpen}
      align="right"
      trigger={({ toggle, ref }) => (
        <button
          ref={ref as (el: HTMLButtonElement | null) => void}
          type="button"
          className="icon-btn"
          title="History"
          aria-label="History"
          onClick={toggle}
        >
          <Icon name="history" />
        </button>
      )}
    >
      <div className="menu history-menu" role="menu">
        <button type="button" className="menu-item" onClick={props.onNew}>
          <Icon name="plus" />
          <span>New session</span>
        </button>
        {threads.length > 0 && <div className="menu-sep" />}
        {threads.length === 0 && <div className="menu-empty">No sessions yet</div>}
        {threads.map((thread) => (
          <button
            key={thread.id}
            type="button"
            className={thread.id === props.selectedThread?.id ? "menu-item selected" : "menu-item"}
            onClick={() => props.onSelect(thread.id)}
          >
            <span className="history-dot" data-status={thread.status} />
            <span className="history-text">
              <span className="history-title">{thread.title}</span>
              <small>
                {labelAgent(thread.active_agent ?? thread.preferred_agent)} · {humanize(thread.status)}
              </small>
            </span>
          </button>
        ))}
      </div>
    </Popover>
  );
}

const APPROVAL_ICON: Record<ApprovalRequest["kind"], "terminal" | "repo" | "agent"> = {
  command: "terminal",
  file_change: "repo",
  tool: "agent",
};

function ApprovalCard(props: { approval: ApprovalRequest; onResolve(decision: ApprovalDecision): void }) {
  const { approval } = props;
  const commandText = approval.command?.join(" ");
  const title =
    approval.kind === "command"
      ? "Run command"
      : approval.kind === "file_change"
        ? "Apply file change"
        : `Use ${approval.tool_name}`;
  return (
    <article className="approval-card">
      <div className="approval-head">
        <span className="approval-badge">
          <Icon name={APPROVAL_ICON[approval.kind]} />
        </span>
        <div className="approval-title">
          <strong>{title}</strong>
          <small>{labelAgent(approval.agent)} needs your approval</small>
        </div>
      </div>
      {commandText && <pre className="approval-cmd"><code>{commandText}</code></pre>}
      {approval.cwd && <div className="approval-meta">in {approval.cwd}</div>}
      {approval.reason && <div className="approval-reason">{approval.reason}</div>}
      <div className="approval-actions">
        <button type="button" className="approval-btn allow" onClick={() => props.onResolve("allow")}>
          <Icon name="check" /> Allow
        </button>
        <button
          type="button"
          className="approval-btn allow-session"
          onClick={() => props.onResolve("allow_for_session")}
        >
          Allow for session
        </button>
        <button type="button" className="approval-btn deny" onClick={() => props.onResolve("deny")}>
          <Icon name="close" /> Deny
        </button>
        <button type="button" className="approval-btn abort" onClick={() => props.onResolve("abort")}>
          <Icon name="stop" /> Stop
        </button>
      </div>
    </article>
  );
}

function Composer(props: {
  snapshot: WorkbenchSnapshot | null;
  selectedThread: AgentThread | null;
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
  const [reposOpen, setReposOpen] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [permOpen, setPermOpen] = useState(false);
  const sandboxAllowed = props.agent === "codex";
  const sandboxOn = props.backend === "docker_sandbox";
  const sandbox = props.snapshot?.sandboxRuntime;
  const repos = props.snapshot?.repos ?? [];
  const selectedRepos = repos.filter((repo) => props.repoIds.includes(repo.id));
  const reposLabel =
    selectedRepos.length === 0
      ? "Connect repos"
      : selectedRepos.length === 1
        ? selectedRepos[0].name
        : `${selectedRepos.length} repos`;
  const reposTitle =
    selectedRepos.length === 0
      ? "Connected repos — none attached yet"
      : `Connected repos: ${selectedRepos.map((repo) => repo.name).join(", ")}`;
  const perm = PERMISSIONS.find((item) => item.value === props.permission) ?? PERMISSIONS[1];
  const optionsActive = sandboxOn || !!props.model.trim() || (!!props.reasoning && props.reasoning !== "medium");

  return (
    <footer className="composer">
      <div className="composer-box">
        <textarea
          value={props.message}
          placeholder={props.isRunning ? "Queue a follow-up…" : "Ask anything — ⌘⏎ to send"}
          rows={1}
          onChange={(event) => props.setMessage(event.target.value)}
          onInput={(event) => autoGrow(event.currentTarget)}
          onKeyDown={(event) => {
            if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
              event.preventDefault();
              props.onSend();
            }
          }}
        />

        <div className="toolbar">
          <div className="toolbar-chips">
            <Popover
              open={reposOpen}
              setOpen={setReposOpen}
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className="chip-btn"
                  title={reposTitle}
                  aria-label="Connected repos"
                  onClick={toggle}
                >
                  <Icon name="repo" />
                  <span className="chip-label">{reposLabel}</span>
                  {selectedRepos.length > 0 && <span className="chip-count">{selectedRepos.length}</span>}
                  <Icon name="caret" className="chip-caret" />
                </button>
              )}
            >
              <div className="menu repo-menu">
                <div className="menu-head">Connected Repos</div>
                {repos.length === 0 && <div className="menu-empty">No repositories connected</div>}
                {repos.map((repo) => {
                  const checked = props.repoIds.includes(repo.id);
                  return (
                    <label key={repo.id} className={checked ? "menu-item check selected" : "menu-item check"}>
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
                      <span className="history-text">
                        <span>{repo.name}</span>
                        <small>{repo.kind === "github" ? "GitHub" : "Local"}</small>
                      </span>
                    </label>
                  );
                })}
                <div className="menu-sep" />
                <button type="button" className="menu-item" onClick={() => { setReposOpen(false); props.onWorkspaceRepos(); }}>
                  <Icon name="folder" />
                  <span>Add local folder</span>
                </button>
                <button type="button" className="menu-item" onClick={() => { setReposOpen(false); props.onGithub(); }}>
                  <Icon name="github" />
                  <span>Add from GitHub</span>
                </button>
              </div>
            </Popover>

            <Dropdown
              ariaLabel="Agent"
              title="Agent"
              icon={<Icon name="agent" />}
              value={props.agent}
              onChange={(value) => props.setAgent(value as AgentKind)}
              options={[
                { value: "claude_code", label: "Claude" },
                { value: "codex", label: "Codex" },
              ]}
            />

            <Popover
              open={permOpen}
              setOpen={setPermOpen}
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className={`icon-chip${props.permission === "autonomous" ? " danger" : ""}`}
                  title={`Permission: ${perm.label}`}
                  aria-label={`Permission mode: ${perm.label}`}
                  onClick={toggle}
                >
                  <Icon name={perm.icon} />
                </button>
              )}
            >
              <div className="menu" role="listbox">
                <div className="menu-head">Permission</div>
                {PERMISSIONS.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    role="option"
                    aria-selected={option.value === props.permission}
                    className={option.value === props.permission ? "menu-item selected" : "menu-item"}
                    onClick={() => {
                      props.setPermission(option.value);
                      setPermOpen(false);
                    }}
                  >
                    <Icon name={option.icon} />
                    <span>{option.label}</span>
                    {option.value === props.permission && <Icon name="check" />}
                  </button>
                ))}
              </div>
            </Popover>

            <Popover
              open={optionsOpen}
              setOpen={setOptionsOpen}
              align="right"
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className={`icon-chip${optionsActive ? " active" : ""}`}
                  title="Model, reasoning & sandbox"
                  aria-label="Run options"
                  onClick={toggle}
                >
                  <Icon name="sliders" />
                </button>
              )}
            >
              <div className="menu options-menu">
                <div className="menu-head">Run options</div>
                <label className="field">
                  <span>Model{props.model.trim() && <em className="field-value">{prettyModel(props.model)}</em>}</span>
                  <input
                    list="am-model-suggestions"
                    value={props.model}
                    placeholder="Default model"
                    onChange={(event) => props.setModel(event.target.value)}
                  />
                  <datalist id="am-model-suggestions">
                    {modelSuggestions(props.agent).map((name) => (
                      <option key={name} value={name} />
                    ))}
                  </datalist>
                </label>
                <label className="field">
                  <span>Reasoning effort</span>
                  <select value={props.reasoning} onChange={(event) => props.setReasoning(event.target.value)}>
                    <option value="">Default</option>
                    <option value="low">Low</option>
                    <option value="medium">Medium</option>
                    <option value="high">High</option>
                  </select>
                </label>
                <button
                  type="button"
                  className={`sandbox-row${sandboxOn ? " on" : ""}${sandboxAllowed ? "" : " disabled"}`}
                  disabled={!sandboxAllowed}
                  aria-pressed={sandboxOn}
                  title={sandboxAllowed ? "Run in an isolated Docker sandbox" : "Sandbox is available for Codex"}
                  onClick={() => props.setBackend(sandboxOn ? "host" : "docker_sandbox")}
                >
                  <Icon name={sandboxOn ? "cube" : "cubeOff"} />
                  <span>Docker sandbox</span>
                  <span className="sandbox-state">{sandboxOn ? "On" : "Off"}</span>
                </button>
                {sandboxOn && sandbox && !sandbox.authenticated && (
                  <button type="button" className="menu-item" onClick={() => props.onSandboxLogin(false)}>
                    Sign in to Sandbox
                  </button>
                )}
                {sandboxOn && sandbox && !sandbox.codex_authenticated && (
                  <button type="button" className="menu-item" onClick={() => props.onSandboxLogin(true)}>
                    Sign in to Codex Sandbox
                  </button>
                )}
              </div>
            </Popover>
          </div>

          <button
            className="send-btn"
            type="button"
            disabled={!props.canSend}
            title={props.isRunning ? "Queue message (⌘⏎)" : "Send (⌘⏎)"}
            aria-label={props.isRunning ? "Queue message" : "Send"}
            onClick={props.onSend}
          >
            <Icon name={props.isRunning ? "queue" : "send"} />
          </button>
        </div>
      </div>
    </footer>
  );
}

const PERMISSIONS: { value: PermissionPolicy; label: string; icon: "eye" | "shield" | "bolt" }[] = [
  { value: "read_only", label: "Read only", icon: "eye" },
  { value: "workspace_write", label: "Write", icon: "shield" },
  { value: "autonomous", label: "Autonomous", icon: "bolt" },
];

function RunDetails(props: {
  details: NonNullable<WorkbenchSnapshot["details"]>;
  onOpenPath(path: string): void;
  onDeleteQueued(id: string): void;
  onMoveQueued(orderedIds: string[]): void;
}) {
  const [open, setOpen] = useState(false);
  const queuedIds = props.details.queued.map((turn) => turn.id);
  const diffFiles =
    props.details.diff?.repos.flatMap((repo) => repo.files.map((file) => ({ ...file, repo: repo.repo_name }))) ?? [];
  const hasContent = props.details.repos.length > 0 || props.details.queued.length > 0 || diffFiles.length > 0;
  if (!hasContent) return null;

  return (
    <section className="run-details">
      <button type="button" className="run-toggle" onClick={() => setOpen(!open)}>
        <Icon name="caret" className={open ? "caret open" : "caret"} />
        <span>Workspace</span>
        <span className="run-summary">
          {props.details.repos.length} repo{props.details.repos.length === 1 ? "" : "s"}
          {props.details.queued.length > 0 && ` · ${props.details.queued.length} queued`}
          {diffFiles.length > 0 && ` · ${diffFiles.length} changed`}
        </span>
      </button>

      {open && (
        <div className="run-body">
          {props.details.repos.map((repo) => (
            <div key={repo.repo_id} className="detail-row">
              <Icon name="repo" />
              <span className="detail-name">{repo.repo_name}</span>
              <small>{repo.branch ?? repo.workspace_backend}</small>
              {repo.worktree_path && (
                <button type="button" className="link-btn" onClick={() => props.onOpenPath(repo.worktree_path!)}>
                  Open
                </button>
              )}
            </div>
          ))}

          {props.details.queued.length > 0 && (
            <div className="queue-list">
              <div className="detail-head">Queued</div>
              {props.details.queued.map((turn, index) => (
                <div className="queue-item" key={turn.id}>
                  <span>{turn.message}</span>
                  <IconButton title="Move up" disabled={index === 0} onClick={() => props.onMoveQueued(move(queuedIds, index, index - 1))}>
                    <Icon name="up" />
                  </IconButton>
                  <IconButton
                    title="Move down"
                    disabled={index === queuedIds.length - 1}
                    onClick={() => props.onMoveQueued(move(queuedIds, index, index + 1))}
                  >
                    <Icon name="down" />
                  </IconButton>
                  <IconButton title="Remove" onClick={() => props.onDeleteQueued(turn.id)}>
                    <Icon name="close" />
                  </IconButton>
                </div>
              ))}
            </div>
          )}

          {diffFiles.length > 0 && (
            <div className="diff-list">
              <div className="detail-head">Changes</div>
              {diffFiles.map((file) => (
                <div key={`${file.repo}:${file.path}`} className="diff-item">
                  <span className="detail-name">{file.path}</span>
                  <span className="diff-add">+{file.additions}</span>
                  <span className="diff-del">-{file.deletions}</span>
                </div>
              ))}
            </div>
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
    <div className="sheet-backdrop" onMouseDown={props.onClose}>
      <section className="sheet" onMouseDown={(event) => event.stopPropagation()}>
        <header>
          <strong>Settings</strong>
          <IconButton title="Close" onClick={props.onClose}>
            <Icon name="close" />
          </IconButton>
        </header>

        <div className="sheet-body">
          <div className="settings-group">
            <div className="group-title">Limit handling</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.auto_switch}
                onChange={(event) => setLimit({ ...limit, auto_switch: event.target.checked })}
              />
              <span>Auto switch agents on limits</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.switch_back}
                onChange={(event) => setLimit({ ...limit, switch_back: event.target.checked })}
              />
              <span>Switch back on recovery</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.resume_with_earliest}
                onChange={(event) => setLimit({ ...limit, resume_with_earliest: event.target.checked })}
              />
              <span>Resume with earliest agent</span>
            </label>
            <div className="field">
              <span>Fallback order</span>
              <div className="segmented">
                <button
                  type="button"
                  className={claudeFirst ? "selected" : ""}
                  onClick={() => setLimit({ ...limit, agent_priority: ["claude_code", "codex"] })}
                >
                  Claude first
                </button>
                <button
                  type="button"
                  className={!claudeFirst ? "selected" : ""}
                  onClick={() => setLimit({ ...limit, agent_priority: ["codex", "claude_code"] })}
                >
                  Codex first
                </button>
              </div>
            </div>
            <label className="field">
              <span>Retry seconds</span>
              <input
                type="number"
                min={0}
                value={limit.unknown_reset_retry_secs}
                onChange={(event) => setLimit({ ...limit, unknown_reset_retry_secs: Number(event.target.value) })}
              />
            </label>
          </div>

          <div className="settings-group">
            <div className="group-title">Docker Sandbox</div>
            <label className="field">
              <span>Default runtime</span>
              <select
                value={sandbox.default_backend}
                onChange={(event) => setSandbox({ ...sandbox, default_backend: event.target.value as ExecutionBackend })}
              >
                <option value="host">Host</option>
                <option value="docker_sandbox">Docker Sandbox</option>
              </select>
            </label>
            <div className="field-grid">
              <label className="field">
                <span>Max sandboxes</span>
                <input
                  type="number"
                  min={1}
                  max={8}
                  value={sandbox.max_concurrent_sandboxes}
                  onChange={(event) => setSandbox({ ...sandbox, max_concurrent_sandboxes: Number(event.target.value) })}
                />
              </label>
              <label className="field">
                <span>CPUs</span>
                <input
                  type="number"
                  min={1}
                  max={16}
                  value={sandbox.cpus}
                  onChange={(event) => setSandbox({ ...sandbox, cpus: Number(event.target.value) })}
                />
              </label>
              <label className="field">
                <span>Memory</span>
                <input value={sandbox.memory} onChange={(event) => setSandbox({ ...sandbox, memory: event.target.value })} />
              </label>
              <label className="field">
                <span>Network</span>
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
          </div>
        </div>

        <footer>
          <button type="button" onClick={props.onOpenSettings}>
            VS Code settings
          </button>
          <button type="button" className="primary" onClick={() => props.onApply(limit, sandbox)}>
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
  const [query, setQuery] = useState("");
  const filtered = props.repos.filter((repo) => repo.full_name.toLowerCase().includes(query.toLowerCase()));
  return (
    <div className="sheet-backdrop" onMouseDown={props.onClose}>
      <section className="sheet repo-sheet" onMouseDown={(event) => event.stopPropagation()}>
        <header>
          <strong>Add from GitHub</strong>
          <IconButton title="Close" onClick={props.onClose}>
            <Icon name="close" />
          </IconButton>
        </header>
        <div className="sheet-search">
          <Icon name="search" />
          <input autoFocus placeholder="Filter repositories" value={query} onChange={(event) => setQuery(event.target.value)} />
        </div>
        <div className="github-list">
          {props.loading && <div className="menu-empty">Loading repositories…</div>}
          {!props.loading && filtered.length === 0 && <div className="menu-empty">No matching repositories</div>}
          {!props.loading &&
            filtered.map((repo) => (
              <button key={repo.id} type="button" className="github-row" onClick={() => props.onConnect(repo)}>
                <Icon name={repo.private ? "lock" : "github"} />
                <span className="history-text">
                  <span>{repo.full_name}</span>
                  <small>{repo.private ? "Private" : "Public"} · {repo.default_branch}</small>
                </span>
              </button>
            ))}
        </div>
      </section>
    </div>
  );
}

function EmptyState({ trusted, compact = false }: { trusted: boolean; compact?: boolean }) {
  return (
    <div className={compact ? "empty compact" : "empty"}>
      <span className="empty-mark">
        <BrandMark size={34} />
      </span>
      <strong>{trusted ? "Start a session" : "Restricted Mode"}</strong>
      <span>
        {trusted ? "Pick an agent and ask anything from the composer below." : "Trust this workspace to run agents."}
      </span>
    </div>
  );
}

function autoGrow(el: HTMLTextAreaElement) {
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
}

function runDefaults(snapshot: WorkbenchSnapshot, agent: AgentKind) {
  return snapshot.runDefaults.find((item) => item.kind === agent) ?? { model: null, reasoning: null };
}

// Valid model identifiers per agent (kept lowercase so they pass straight to the CLI).
function modelSuggestions(agent: AgentKind): string[] {
  return agent === "codex"
    ? ["gpt-5-codex", "gpt-5", "gpt-4.1", "o3", "o4-mini"]
    : ["opus", "sonnet", "haiku"];
}

// Display-only: turn a raw model id into a properly-capitalized name.
function prettyModel(value: string): string {
  const v = value.trim();
  if (!v) return "";
  const known: Record<string, string> = {
    opus: "Opus",
    sonnet: "Sonnet",
    haiku: "Haiku",
    "claude-opus-4-8": "Claude Opus 4.8",
    "claude-sonnet-4-6": "Claude Sonnet 4.6",
    "claude-haiku-4-5": "Claude Haiku 4.5",
    "gpt-5": "GPT-5",
    "gpt-5-codex": "GPT-5 Codex",
    "gpt-4.1": "GPT-4.1",
    o3: "o3",
    "o4-mini": "o4-mini",
  };
  const hit = known[v.toLowerCase()];
  if (hit) return hit;
  return v.replace(/\bgpt\b/gi, "GPT").replace(/(^|[\s\-_])([a-z])/g, (_match, sep, ch) => sep + ch.toUpperCase());
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

function roleLabel(role: string, kind: string, agent: AgentKind): string {
  if (role === "user") return "You";
  if (role === "assistant") return labelAgent(agent);
  if (role === "system" || !role) return humanize(kind || "system");
  return humanize(role);
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
