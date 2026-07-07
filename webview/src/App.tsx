import { Fragment, memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type {
  AgentKind,
  AgentModelOption,
  AgentThread,
  AgentThreadEvent,
  ApprovalDecision,
  ApprovalRequest,
  AvailabilityState,
  CloudPolicy,
  ExecutionBackend,
  ExtensionMessage,
  GithubRepository,
  LimitPolicy,
  LocalModelProvider,
  PermissionPolicy,
  SandboxPolicy,
  WorkbenchSnapshot,
} from "./types";
import { BrandMark, Icon } from "./icons";
import { Markdown } from "./markdown";

type PendingMessage = { id: string; text: string };
type PersistedState = {
  repoIds?: string[];
  repoTouched?: boolean;
};

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

function readPersistedState(): PersistedState {
  const state = vscode.getState();
  return state && typeof state === "object" ? (state as PersistedState) : {};
}

function writePersistedState(state: PersistedState): void {
  vscode.setState(state);
}

export default function App() {
  const persisted = useMemo(() => readPersistedState(), []);
  const [snapshot, setSnapshot] = useState<WorkbenchSnapshot | null>(null);
  const [agent, setAgent] = useState<AgentKind>("claude_code");
  const [permission, setPermission] = useState<PermissionPolicy>("workspace_write");
  const [backend, setBackend] = useState<ExecutionBackend>("host");
  const [model, setModel] = useState("");
  const [reasoning, setReasoning] = useState("medium");
  const [localProvider, setLocalProvider] = useState<LocalModelProvider | "">("");
  const [localBaseUrl, setLocalBaseUrl] = useState("");
  const [repoIds, setRepoIds] = useState<string[]>(persisted.repoIds ?? []);
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
  const runControlsKeyRef = useRef<string>("");
  const handoffRef = useRef<Record<string, string>>({});
  const repoTouchedRef = useRef(!!persisted.repoTouched);
  const repoInitKeyRef = useRef("");

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
        const selected = incoming.snapshot.threads.find((thread) => thread.id === incoming.snapshot.selectedThreadId);
        setPending((prev) =>
          prev.filter(
            (item) =>
              !selected ||
              selected.status === "draft" ||
              (!events.some((e) => e.role === "user" && (e.text ?? "").trim() === item.text) &&
                !queued.some((q) => q.message.trim() === item.text) &&
                !events.some((e) => e.kind === "user_message" && (e.text ?? "").trim() === item.text))
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
      if (incoming.type === "repoConnected") {
        repoTouchedRef.current = true;
        setRepoIds((prev) => (prev.includes(incoming.repo.id) ? prev : [...prev, incoming.repo.id]));
        setNotice(`Connected ${incoming.repo.name}.`);
        return;
      }
      if (incoming.type === "notice" || incoming.type === "error") {
        setNotice(incoming.message);
        if (incoming.type === "error") setPending([]);
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

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 6500);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    writePersistedState({
      ...readPersistedState(),
      repoIds,
      repoTouched: repoTouchedRef.current,
    });
  }, [repoIds]);

  const effectiveSelectedId = navThreadId !== undefined ? navThreadId : snapshot?.selectedThreadId ?? null;
  const selectedThread = useMemo(
    () => snapshot?.threads.find((thread) => thread.id === effectiveSelectedId) ?? null,
    [snapshot, effectiveSelectedId]
  );
  // Details belong to the daemon's current selection; while navigating to a
  // different thread they're stale, so we show a loading state instead.
  const navigating = navThreadId !== undefined && navThreadId !== null && navThreadId !== (snapshot?.selectedThreadId ?? null);
  useEffect(() => {
    if (!snapshot) return;
    const nextAgent = selectedThread?.active_agent ?? selectedThread?.preferred_agent ?? snapshot.defaults.agent;
    const defaults = runDefaults(snapshot, nextAgent);
    const nextPermission = selectedThread?.permission ?? snapshot.defaults.permission;
    const nextBackend = sanitizeBackend(nextAgent, selectedThread?.execution_backend ?? snapshot.defaults.execution_backend);
    const nextModel = selectedThread?.model ?? snapshot.defaults.model ?? defaults.model ?? "";
    const nextReasoning = selectedThread?.reasoning ?? snapshot.defaults.reasoning ?? defaults.reasoning ?? "medium";
    const nextLocalProvider = selectedThread?.local_provider ?? snapshot.defaults.local_provider ?? "";
    const nextLocalBaseUrl = selectedThread?.local_base_url ?? snapshot.defaults.local_base_url ?? "";
    const runControlsKey = [
      effectiveSelectedId ?? "new",
      nextAgent,
      nextPermission,
      nextBackend,
      nextModel,
      nextReasoning,
      nextLocalProvider,
      nextLocalBaseUrl,
      selectedThread?.handoff_state ?? "",
    ].join("|");
    if (runControlsKeyRef.current !== runControlsKey) {
      runControlsKeyRef.current = runControlsKey;
      setAgent(nextAgent);
      setPermission(nextPermission);
      setBackend(nextBackend);
      setModel(nextModel);
      setReasoning(nextReasoning);
      setLocalProvider(nextAgent === "codex" ? nextLocalProvider : "");
      setLocalBaseUrl(nextAgent === "codex" ? nextLocalBaseUrl : "");
    }
    if (selectedThread && snapshot.details?.repos.length) {
      setRepoIds(snapshot.details.repos.map((repo) => repo.repo_id));
    } else if (!selectedThread) {
      const repoKey = snapshot.repos.map((repo) => repo.id).join("|");
      if (!repoTouchedRef.current && repoInitKeyRef.current !== repoKey) {
        repoInitKeyRef.current = repoKey;
        setRepoIds(snapshot.defaultRepoIds ?? []);
      }
    }
  }, [
    effectiveSelectedId,
    selectedThread?.active_agent,
    selectedThread?.preferred_agent,
    selectedThread?.permission,
    selectedThread?.execution_backend,
    selectedThread?.model,
    selectedThread?.reasoning,
    selectedThread?.local_provider,
    selectedThread?.local_base_url,
    selectedThread?.handoff_state,
    snapshot?.defaults.agent,
    snapshot?.defaults.permission,
    snapshot?.defaults.execution_backend,
    snapshot?.defaults.model,
    snapshot?.defaults.reasoning,
    snapshot?.defaults.local_provider,
    snapshot?.defaults.local_base_url,
    snapshot?.repos,
    snapshot?.defaultRepoIds,
    snapshot?.runDefaults,
  ]);

  useEffect(() => {
    if (!selectedThread) return;
    const fallback = selectedThread.fallback_agent;
    const active = selectedThread.active_agent;
    const original = selectedThread.original_agent;
    const isFallbackActive =
      !!fallback && active === fallback && (selectedThread.handoff_state === "fallback_active" || !!original);
    const key = isFallbackActive
      ? `${selectedThread.id}:${original ?? "agent"}:${fallback}:${selectedThread.model ?? ""}`
      : `${selectedThread.id}:none`;
    if (handoffRef.current[selectedThread.id] === key) return;
    handoffRef.current[selectedThread.id] = key;
    if (!isFallbackActive) return;
    const modelNote = selectedThread.original_model !== selectedThread.model
      ? ` (${formatModelSwitch(selectedThread.original_model, selectedThread.model)})`
      : "";
    setNotice(`Rate Limit reached, switched to ${labelAgent(fallback)}${modelNote}.`);
  }, [
    selectedThread?.id,
    selectedThread?.active_agent,
    selectedThread?.original_agent,
    selectedThread?.fallback_agent,
    selectedThread?.handoff_state,
    selectedThread?.model,
    selectedThread?.original_model,
  ]);

  useEffect(() => {
    transcriptRef.current?.scrollTo({
      top: transcriptRef.current.scrollHeight,
      behavior: "auto",
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
        const activeFallback = selectedThread?.fallback_agent && selectedThread.active_agent === selectedThread.fallback_agent;
        if (activeFallback && status.kind === selectedThread?.original_agent) continue;
        const until = status.reset_at ? ` until ${formatTime(status.reset_at)}` : "";
        setNotice(`${labelAgent(status.kind)} is rate limited${until}.`);
      } else if (previous === "limited" && status.availability === "available") {
        setNotice(`${labelAgent(status.kind)} is available again.`);
      }
    }
  }, [snapshot]);

  const isRunning = selectedThread?.status === "running";
  const details = navigating ? undefined : snapshot?.details;
  const reposLocked = !!selectedThread && !!details?.repos.some((repo) => !!repo.worktree_path);
  const canReviewChanges = !!selectedThread && !!details?.repos.some((repo) => !!repo.worktree_path);

  // The composer owns its own draft text so typing never re-renders the
  // transcript; it hands us the final text here on submit.
  const send = (raw: string) => {
    const text = raw.trim();
    if (!text || !snapshot?.trusted) return;
    const validRepoIds = snapshot.repos.filter((repo) => repoIds.includes(repo.id)).map((repo) => repo.id);
    if (snapshot.repos.length > 0 && validRepoIds.length === 0) {
      setNotice("Select at least one connected repository before starting the agent.");
      return;
    }
    setPending((prev) => [...prev, { id: `pending-${Date.now()}-${prev.length}`, text }]);
    vscode.postMessage({
      type: "submit",
      message: text,
      threadId: selectedThread?.id ?? null,
      repoIds: validRepoIds,
      agent,
      permission,
      executionBackend: sanitizeBackend(agent, backend),
      model: model.trim() || null,
      reasoning: reasoning.trim() || null,
      localProvider: localProvider || null,
      localBaseUrl: localBaseUrl.trim() || null,
    });
  };

  const pickAgent = (nextAgent: AgentKind) => {
    setAgent(nextAgent);
    if (nextAgent !== "codex") {
      setBackend("host");
      setLocalProvider("");
      setLocalBaseUrl("");
    }
    if (!snapshot) return;
    const defaults = runDefaults(snapshot, nextAgent);
    if (!model.trim()) setModel(defaults.model ?? "");
    if (!reasoning.trim()) setReasoning(defaults.reasoning ?? "medium");
  };

  const setDraftRepoIds = (next: string[]) => {
    repoTouchedRef.current = true;
    setRepoIds(next);
    if (selectedThread && !reposLocked) {
      vscode.postMessage({ type: "assignRepos", threadId: selectedThread.id, repoIds: next });
    }
  };

  const newSession = () => {
    setHistoryOpen(false);
    setPending([]);
    // Reflect the empty composer instantly instead of waiting for the round-trip.
    setNavThreadId(null);
    vscode.postMessage({ type: "newSession" });
  };

  const selectThread = (id: string) => {
    setHistoryOpen(false);
    setPending([]);
    setNavThreadId(id);
    vscode.postMessage({ type: "selectThread", threadId: id });
  };

  const deleteThread = (id: string, force: boolean) => {
    // Deleting clears the daemon's selection; if we're removing the open thread,
    // jump to a fresh session optimistically so the view doesn't flash stale.
    if (id === effectiveSelectedId) {
      setPending([]);
      setNavThreadId(null);
    }
    vscode.postMessage({ type: "deleteThread", threadId: id, force });
  };

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <div className="brand-text">
            <strong>{selectedThread?.title ?? "New session"}</strong>
          </div>
        </div>
        <div className="top-actions">
          <HistoryMenu
            open={historyOpen}
            setOpen={setHistoryOpen}
            snapshot={snapshot}
            selectedThread={selectedThread}
            onNew={newSession}
            onSelect={selectThread}
            onDelete={deleteThread}
          />
          <IconButton title="Open in editor" onClick={() => vscode.postMessage({ type: "openPanel" })}>
            <Icon name="window" />
          </IconButton>
          <IconButton title="Settings" onClick={() => setSettingsOpen(true)}>
            <Icon name="settings" />
          </IconButton>
          {canReviewChanges && (
            <IconButton
              title="Review changes"
              onClick={() => vscode.postMessage({ type: "loadDiff", threadId: selectedThread.id })}
            >
              <Icon name="repo" />
            </IconButton>
          )}
          <IconButton title="Refresh" onClick={() => vscode.postMessage({ type: "refresh" })}>
            <Icon name="refresh" />
          </IconButton>
          <IconButton title="New session" onClick={newSession}>
            <Icon name="plus" />
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
              <Fragment key={event.id}>
                <MessageView event={event} />
              </Fragment>
            ))}
          {selectedThread && details && (
            <ChangesView
              threadId={selectedThread.id}
              diff={details.diff}
              diffState={details.diffState ?? "idle"}
              repos={details.repos}
              applyResult={details.applyResult ?? null}
              onLoadDiff={(threadId) => vscode.postMessage({ type: "loadDiff", threadId })}
              onApply={(threadId) => vscode.postMessage({ type: "applyThreadChanges", threadId })}
              onOpenPath={(target) => vscode.postMessage({ type: "openPath", path: target })}
            />
          )}
          {pending.map((item) => (
            <article key={item.id} className="msg user pending">
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
          {!navigating && selectedThread && (isRunning || pending.length > 0) && !details?.approvals.length && (
            <div className="thinking">Working…</div>
          )}
          {!navigating &&
            selectedThread &&
            details &&
            details.events.length === 0 &&
            pending.length === 0 &&
            !isRunning && <EmptyState trusted={snapshot?.trusted ?? true} compact />}
        </div>

      </section>

      <Composer
        snapshot={snapshot}
        selectedThread={selectedThread}
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
        localProvider={localProvider}
        setLocalProvider={setLocalProvider}
        localBaseUrl={localBaseUrl}
        setLocalBaseUrl={setLocalBaseUrl}
        repoIds={repoIds}
        setRepoIds={setDraftRepoIds}
        reposLocked={reposLocked}
        isRunning={isRunning}
        onSend={send}
        onStop={() => selectedThread && vscode.postMessage({ type: "stopThread", threadId: selectedThread.id })}
        onGithub={() => {
          setGithubLoading(true);
          vscode.postMessage({ type: "githubList" });
        }}
        onLocalRepo={() => vscode.postMessage({ type: "connectLocalRepo" })}
        onSandboxLogin={(codex) => vscode.postMessage({ type: "sandboxLogin", codex })}
      />

      {settingsOpen && snapshot && (
        <SettingsSheet
          snapshot={snapshot}
          onClose={() => setSettingsOpen(false)}
          onApply={(limitPolicy, sandboxPolicy, cloudPolicy) => {
            vscode.postMessage({ type: "setLimitPolicy", policy: limitPolicy });
            vscode.postMessage({ type: "setSandboxPolicy", policy: sandboxPolicy });
            vscode.postMessage({ type: "setCloudPolicy", policy: cloudPolicy });
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

// One transcript row. Memoized so a snapshot tick only re-renders the messages
// that actually changed — an unchanged message keeps its parsed Markdown.
const MessageView = memo(function MessageView({ event }: { event: AgentThreadEvent }) {
  if (isActivityEvent(event)) {
    const detail = activityDetail(event);
    return (
      <article className="activity-row">
        <span className="activity-icon">
          <Icon name={activityIcon(event)} />
        </span>
        <div className="activity-main">
          <div className="activity-title">
            <span>{activitySummary(event)}</span>
          </div>
          {detail && (
            <details className="activity-detail">
              <summary>Details</summary>
              <pre>{detail}</pre>
            </details>
          )}
        </div>
      </article>
    );
  }
  return (
    <article className={`msg ${messageClass(event.role)}`}>
      <div className="msg-body">{event.text ? <Markdown text={event.text} /> : humanize(event.kind)}</div>
    </article>
  );
});

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
  align?: "left" | "right" | "center";
  placement?: "auto" | "above" | "below";
  children: ReactNode;
}) {
  const triggerRef = useRef<HTMLElement | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);
  const align = props.align ?? "left";
  const placement = props.placement ?? "auto";

  useLayoutEffect(() => {
    if (!props.open) {
      setMenuStyle(null);
      return;
    }
    const reposition = () => {
      const trigger = triggerRef.current;
      if (!trigger) return;
      const rect = trigger.getBoundingClientRect();
      const margin = 6;
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const spaceBelow = vh - rect.bottom - margin;
      const spaceAbove = rect.top - margin;
      const openUp = placement === "above" || (placement === "auto" && spaceBelow < 220 && spaceAbove > spaceBelow);
      const availableHeight = openUp ? spaceAbove : spaceBelow;
      const maxHeight = Math.max(120, Math.min(360, availableHeight));
      const maxWidth = vw - margin * 2;
      const menuWidth = Math.min(menuRef.current?.offsetWidth ?? rect.width, maxWidth);
      const menuHeight = Math.min(menuRef.current?.offsetHeight ?? maxHeight, maxHeight);
      // Anchor by preferred edge, then clamp fully inside the viewport so menus never clip.
      const preferredLeft =
        align === "center" ? (vw - menuWidth) / 2 : align === "right" ? rect.right - menuWidth : rect.left;
      const left = Math.max(margin, Math.min(preferredLeft, vw - menuWidth - margin));
      const preferredTop = openUp ? rect.top - menuHeight - margin : rect.bottom + margin;
      const top = Math.max(margin, Math.min(preferredTop, vh - menuHeight - margin));
      const next: CSSProperties = {
        position: "fixed",
        maxHeight,
        maxWidth,
        minWidth: Math.min(rect.width, maxWidth),
        left,
        top,
      };
      setMenuStyle(next);
    };
    reposition();
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    return () => {
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [props.open, align, placement]);

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
        <div
          ref={menuRef}
          className="popover"
          style={menuStyle ?? { position: "fixed", top: 0, left: 0, visibility: "hidden", pointerEvents: "none" }}
        >
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
  className?: string;
  placement?: "auto" | "above" | "below";
}) {
  const [open, setOpen] = useState(false);
  const selected = props.options.find((option) => option.value === props.value);
  return (
    <Popover
      open={open}
      setOpen={setOpen}
      placement={props.placement}
      trigger={({ toggle, ref }) => (
        <button
          ref={ref as (el: HTMLButtonElement | null) => void}
          type="button"
          className={props.className ?? "chip-btn"}
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

function ModelPicker(props: {
  agent: AgentKind;
  snapshot: WorkbenchSnapshot | null;
  localProvider: LocalModelProvider | null;
  value: string;
  onChange(value: string): void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const options = useMemo(
    () => modelOptions(props.agent, props.snapshot, props.localProvider, props.value),
    [props.agent, props.snapshot, props.localProvider, props.value]
  );
  const normalizedQuery = query.trim().toLowerCase();
  const selected = options.find((option) => modelIdsEqual(option.value, props.value));
  const filtered = normalizedQuery
    ? options.filter(
        (option) =>
          option.value.toLowerCase().includes(normalizedQuery) ||
          option.label.toLowerCase().includes(normalizedQuery) ||
          option.source.toLowerCase().includes(normalizedQuery)
      )
    : options;
  const custom = query.trim();
  const canUseCustom = !!custom && !options.some((option) => modelIdsEqual(option.value, custom));

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  return (
    <label className="field">
      <span>
        Model
        {props.value.trim() && prettyModel(props.value) !== props.value.trim() && (
          <em className="field-value">{prettyModel(props.value)}</em>
        )}
      </span>
      <Popover
        open={open}
        setOpen={setOpen}
        placement="above"
        trigger={({ toggle, ref }) => (
          <button
            ref={ref as (el: HTMLButtonElement | null) => void}
            type="button"
            className="model-trigger"
            aria-haspopup="listbox"
            aria-expanded={open}
            onClick={toggle}
          >
            <span>{selected?.label ?? (props.value ? prettyModel(props.value) : "Default model")}</span>
            <Icon name="caret" />
          </button>
        )}
      >
        <div className="menu model-menu" role="listbox">
          <input
            className="model-search"
            autoFocus
            value={query}
            placeholder="Search or type a model id"
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && custom) {
                props.onChange(custom);
                setOpen(false);
              }
            }}
          />
          <button
            type="button"
            role="option"
            aria-selected={!props.value.trim()}
            className={!props.value.trim() ? "menu-item selected" : "menu-item"}
            onClick={() => {
              props.onChange("");
              setOpen(false);
            }}
          >
            <span className="history-text">
              <span>Default model</span>
              <small>Use the installed CLI default</small>
            </span>
            {!props.value.trim() && <Icon name="check" />}
          </button>
          {canUseCustom && (
            <button
              type="button"
              className="menu-item"
              onClick={() => {
                props.onChange(custom);
                setOpen(false);
              }}
            >
              <Icon name="plus" />
              <span className="history-text">
                <span>{custom}</span>
                <small>Use custom model id</small>
              </span>
            </button>
          )}
          {filtered.map((option) => (
            <button
              key={`${option.source}:${option.value}`}
              type="button"
              role="option"
              aria-selected={modelIdsEqual(option.value, props.value)}
              className={modelIdsEqual(option.value, props.value) ? "menu-item selected" : "menu-item"}
              onClick={() => {
                props.onChange(option.value);
                setOpen(false);
              }}
            >
              <span className="history-text">
                <span>{option.label}</span>
                <small>{option.source}</small>
              </span>
              {modelIdsEqual(option.value, props.value) && <Icon name="check" />}
            </button>
          ))}
        </div>
      </Popover>
    </label>
  );
}

function HistoryMenu(props: {
  open: boolean;
  setOpen(open: boolean): void;
  snapshot: WorkbenchSnapshot | null;
  selectedThread: AgentThread | null;
  onNew(): void;
  onSelect(id: string): void;
  onDelete(id: string, force: boolean): void;
}) {
  const threads = props.snapshot?.threads ?? [];
  return (
    <Popover
      open={props.open}
      setOpen={props.setOpen}
      align="center"
      trigger={({ toggle, ref }) => (
        <button
          ref={ref as (el: HTMLButtonElement | null) => void}
          type="button"
          className="icon-btn"
          title="Sessions"
          aria-label="Sessions"
          onClick={toggle}
        >
          <Icon name="history" />
        </button>
      )}
    >
      <div className="menu history-menu" role="menu">
        <div className="history-menu-head">
          <strong>Sessions</strong>
          <button type="button" className="history-new" title="New session" aria-label="New session" onClick={props.onNew}>
            <Icon name="plus" />
          </button>
        </div>
        {threads.length === 0 && <div className="menu-empty">No sessions yet</div>}
        {threads.map((thread) => {
          const running = thread.status === "running";
          const selected = thread.id === props.selectedThread?.id;
          return (
            <div key={thread.id} className={selected ? "history-row selected" : "history-row"}>
              <button
                type="button"
                className="history-pick"
                onClick={() => props.onSelect(thread.id)}
                title={thread.title}
              >
                <span className={running ? "history-spinner" : "history-dot"} data-status={thread.status} />
                <span className="history-text">
                  <span className="history-title">{thread.title}</span>
                  <small>
                    {labelAgent(thread.active_agent ?? thread.preferred_agent)} · {humanize(thread.status)}
                  </small>
                </span>
              </button>
              <button
                type="button"
                className="history-del"
                title={running ? "Stop and delete session" : "Delete session"}
                aria-label="Delete session"
                onClick={(event) => {
                  event.stopPropagation();
                  props.onDelete(thread.id, running);
                }}
              >
                <Icon name="trash" />
              </button>
            </div>
          );
        })}
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
        <div className="approval-title">
          <span className="approval-kicker">
            <Icon name={APPROVAL_ICON[approval.kind]} />
            Approval request
          </span>
          <strong>{title}</strong>
          <small>{labelAgent(approval.agent)} needs your approval</small>
        </div>
      </div>
      {commandText && <pre className="approval-cmd"><code>{commandText}</code></pre>}
      {approval.cwd && <div className="approval-meta">in {approval.cwd}</div>}
      {approval.reason && <div className="approval-reason">{approval.reason}</div>}
      <div className="approval-actions">
        <button type="button" className="approval-btn deny" title="Deny this request" onClick={() => props.onResolve("deny")}>
          <Icon name="close" />
          <span>Deny</span>
        </button>
        <button
          type="button"
          className="approval-btn session"
          title="Allow for the rest of this session"
          onClick={() => props.onResolve("allow_for_session")}
        >
          <Icon name="shield" />
          <span>Session</span>
        </button>
        <button type="button" className="approval-btn allow" title="Allow once" onClick={() => props.onResolve("allow")}>
          <Icon name="check" />
          <span>Allow</span>
        </button>
      </div>
    </article>
  );
}

function Composer(props: {
  snapshot: WorkbenchSnapshot | null;
  selectedThread: AgentThread | null;
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
  localProvider: LocalModelProvider | "";
  setLocalProvider(value: LocalModelProvider | ""): void;
  localBaseUrl: string;
  setLocalBaseUrl(value: string): void;
  repoIds: string[];
  setRepoIds(value: string[]): void;
  reposLocked: boolean;
  isRunning: boolean;
  onSend(text: string): void;
  onStop(): void;
  onGithub(): void;
  onLocalRepo(): void;
  onSandboxLogin(codex: boolean): void;
}) {
  const [reposOpen, setReposOpen] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [permOpen, setPermOpen] = useState(false);
  // The draft lives here, not in App, so each keystroke re-renders only the
  // composer — never the transcript. App receives the text only on submit.
  const [draft, setDraft] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const localOn = !!props.localProvider;
  const localAllowed = props.agent === "codex";
  const sandboxAllowed = props.agent === "codex";
  const sandboxOn = props.backend === "docker_sandbox";
  const sandbox = props.snapshot?.sandboxRuntime;
  const repos = props.snapshot?.repos ?? [];
  const selectedRepos = repos.filter((repo) => props.repoIds.includes(repo.id));
  const noRepoSelected = repos.length > 0 && selectedRepos.length === 0;
  const canSend = !!draft.trim() && !!props.snapshot?.trusted && !noRepoSelected && (!localOn || !!props.model.trim());
  // While the agent is running and the composer is empty, the action button
  // turns into a Stop control (matching Claude Code / Codex). Typing turns it
  // back into a send/queue button.
  const stopMode = props.isRunning && !draft.trim();
  const submit = () => {
    if (!canSend) return;
    props.onSend(draft);
    setDraft("");
    const el = textareaRef.current;
    if (el) el.style.height = "auto";
  };
  const reposLabel =
    selectedRepos.length === 0
      ? "Connect repos"
      : selectedRepos.length === 1
        ? selectedRepos[0].name
        : `${selectedRepos.length} repos`;
  const reposTitle =
    noRepoSelected
      ? "Select a repository for this run"
      : selectedRepos.length === 0
      ? "Connected repos — none attached yet"
      : `Connected repos: ${selectedRepos.map((repo) => repo.name).join(", ")}`;
  const perm = PERMISSIONS.find((item) => item.value === props.permission) ?? PERMISSIONS[1];
  const permissionLabel = permissionComposerLabel(props.permission);
  const optionsActive = sandboxOn || localOn || !!props.model.trim() || (!!props.reasoning && props.reasoning !== "medium");
  const localBaseUrl = props.localBaseUrl.trim() || defaultLocalBaseUrl(props.localProvider || "ollama");

  return (
    <footer className="composer">
      <div className="composer-box">
        <textarea
          ref={textareaRef}
          value={draft}
          placeholder={props.isRunning ? "Ask for follow-up changes" : "Ask anything"}
          rows={1}
          onChange={(event) => setDraft(event.target.value)}
          onInput={(event) => autoGrow(event.currentTarget)}
          onKeyDown={(event) => {
            // Enter sends; Shift+Enter (or ⌘/Ctrl+Enter) inserts a newline.
            if (event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              submit();
            }
          }}
        />

        <div className="toolbar">
          <div className="toolbar-chips">
            <Popover
              open={reposOpen}
              setOpen={setReposOpen}
              placement="above"
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className={`composer-icon-btn${noRepoSelected ? " warning" : ""}`}
                  title={reposTitle}
                  aria-label={reposLabel}
                  onClick={toggle}
                >
                  <Icon name="plus" />
                  {selectedRepos.length > 0 && <span className="context-dot" />}
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
                        disabled={props.reposLocked}
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
                <button type="button" className="menu-item" onClick={() => { setReposOpen(false); props.onLocalRepo(); }}>
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
              className="chip-btn agent-chip"
              icon={<BrandMark size={15} />}
              value={props.agent}
              placement="above"
              onChange={(value) => props.setAgent(value as AgentKind)}
              options={[
                { value: "claude_code", label: "Claude" },
                { value: "codex", label: "Codex" },
              ]}
            />

            <Popover
              open={permOpen}
              setOpen={setPermOpen}
              placement="above"
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className={`permission-chip${props.permission === "autonomous" ? " danger" : ""}`}
                  title={`Permission: ${perm.label}`}
                  aria-label={`Permission mode: ${perm.label}`}
                  onClick={toggle}
                >
                  <Icon name={perm.icon} />
                  <span>{permissionLabel}</span>
                  <Icon name="caret" className="chip-caret" />
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
              placement="above"
              trigger={({ toggle, ref }) => (
                <button
                  ref={ref as (el: HTMLButtonElement | null) => void}
                  type="button"
                  className={`composer-icon-btn${optionsActive ? " active" : ""}`}
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
                <ModelPicker
                  agent={props.agent}
                  snapshot={props.snapshot}
                  localProvider={props.localProvider || null}
                  value={props.model}
                  onChange={props.setModel}
                />
                <label className="field">
                  <span>Reasoning effort</span>
                  <select value={props.reasoning} onChange={(event) => props.setReasoning(event.target.value)}>
                    {reasoningOptions(props.agent, props.snapshot).map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
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
                <button
                  type="button"
                  className={`sandbox-row${localOn ? " on" : ""}${localAllowed ? "" : " disabled"}`}
                  disabled={!localAllowed}
                  aria-pressed={localOn}
                  title={localAllowed ? "Run Codex against a local Ollama or LM Studio model" : "Local model runs use Codex"}
                  onClick={() => {
                    if (localOn) {
                      props.setLocalProvider("");
                      props.setLocalBaseUrl("");
                    } else {
                      const provider = props.snapshot?.defaults.local_provider ?? "ollama";
                      props.setLocalProvider(provider);
                      props.setLocalBaseUrl(props.snapshot?.defaults.local_base_url ?? defaultLocalBaseUrl(provider));
                    }
                  }}
                >
                  <Icon name="terminal" />
                  <span>Local model</span>
                  <span className="sandbox-state">{localOn ? labelLocalProvider(props.localProvider) : "Off"}</span>
                </button>
                {localOn && (
                  <>
                    <label className="field">
                      <span>Local provider</span>
                      <select
                        value={props.localProvider}
                        onChange={(event) => {
                          const provider = event.target.value as LocalModelProvider;
                          props.setLocalProvider(provider);
                          props.setLocalBaseUrl(defaultLocalBaseUrl(provider));
                        }}
                      >
                        <option value="ollama">Ollama</option>
                        <option value="lm_studio">LM Studio</option>
                      </select>
                    </label>
                    <label className="field">
                      <span>Local endpoint</span>
                      <input
                        value={props.localBaseUrl}
                        placeholder={localBaseUrl}
                        onChange={(event) => props.setLocalBaseUrl(event.target.value)}
                      />
                    </label>
                  </>
                )}
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
            className={`send-btn${stopMode ? " stop" : ""}`}
            type="button"
            disabled={!stopMode && !canSend}
            title={stopMode ? "Stop the agent" : props.isRunning ? "Queue message (Enter)" : "Send (Enter)"}
            aria-label={stopMode ? "Stop" : props.isRunning ? "Queue message" : "Send"}
            onClick={stopMode ? props.onStop : submit}
          >
            <Icon name={stopMode ? "stop" : props.isRunning ? "queue" : "send"} />
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

function permissionComposerLabel(permission: PermissionPolicy): string {
  switch (permission) {
    case "read_only":
      return "Read only";
    case "autonomous":
      return "Full access";
    case "workspace_write":
      return "Write access";
  }
}

function ChangesView(props: {
  threadId: string;
  diff: NonNullable<WorkbenchSnapshot["details"]>["diff"];
  diffState: NonNullable<WorkbenchSnapshot["details"]>["diffState"];
  repos: NonNullable<WorkbenchSnapshot["details"]>["repos"];
  applyResult: NonNullable<WorkbenchSnapshot["details"]>["applyResult"] | null;
  onLoadDiff(threadId: string): void;
  onApply(threadId: string): void;
  onOpenPath(path: string): void;
}) {
  const [open, setOpen] = useState(false);
  const diffFiles = props.diff?.repos.flatMap((repo) => repo.files.map((file) => ({ ...file, repo: repo.repo_name }))) ?? [];
  const hasWorktree = props.repos.some((repo) => !!repo.worktree_path);
  if (!hasWorktree || (props.diffState === "idle" && !props.applyResult)) return null;
  const loading = props.diffState === "loading";
  const loaded = props.diffState === "ready";
  const blockers = props.applyResult?.blockers ?? [];
  const additions = diffFiles.reduce((sum, file) => sum + file.additions, 0);
  const deletions = diffFiles.reduce((sum, file) => sum + file.deletions, 0);
  const summary = loaded
    ? `${diffFiles.length} file${diffFiles.length === 1 ? "" : "s"} changed`
    : loading
      ? "Loading changes"
      : "Changes";

  return (
    <section className="changes-card">
      <button type="button" className="changes-toggle" onClick={() => setOpen(!open)}>
        <Icon name="caret" className={open ? "caret open" : "caret"} />
        <span className="changes-title">{summary}</span>
        {loaded && diffFiles.length > 0 && (
          <span className="changes-stats">
            <span className="diff-add">+{additions}</span>
            <span className="diff-del">-{deletions}</span>
          </span>
        )}
        <span className="changes-summary">
          {open ? "Hide" : loaded ? "Review" : "Load diff"}
        </span>
      </button>

      {open && (
        <div className="changes-body">
          <div className="changes-actions">
            <button
              type="button"
              className="secondary-btn"
              disabled={loading}
              onClick={() => props.onLoadDiff(props.threadId)}
            >
              <Icon name="refresh" />
              <span>{loaded ? "Reload Diff" : "Load Diff"}</span>
            </button>
            <button
              type="button"
              className="primary-btn"
              disabled={loading || (loaded && diffFiles.length === 0)}
              onClick={() => props.onApply(props.threadId)}
            >
              <Icon name="check" />
              <span>Apply to Repo</span>
            </button>
          </div>

          {props.repos.map((repo) => (
            <div key={repo.repo_id} className="detail-row">
              <Icon name="repo" />
              <span className="detail-name">{repo.repo_name}</span>
              <small>{repo.branch ?? repo.workspace_backend}</small>
              {repo.worktree_path && (
                <button type="button" className="link-btn" onClick={() => props.onOpenPath(repo.worktree_path!)}>
                  Open Worktree
                </button>
              )}
            </div>
          ))}

          {loading && <div className="menu-empty">Loading diff...</div>}
          {props.diffState === "error" && <div className="menu-empty">Could not load the diff.</div>}
          {loaded && diffFiles.length === 0 && <div className="menu-empty">No changes to apply.</div>}
          {diffFiles.length > 0 && (
            <div className="diff-list">
              {diffFiles.map((file) => (
                <div key={`${file.repo}:${file.path}`} className="diff-item">
                  <span className="detail-name">{file.path}</span>
                  <span className="diff-add">+{file.additions}</span>
                  <span className="diff-del">-{file.deletions}</span>
                </div>
              ))}
            </div>
          )}

          {props.applyResult && (
            <div className={props.applyResult.applied ? "apply-result ok" : "apply-result blocked"}>
              <strong>{props.applyResult.applied ? "Applied to visible repo" : "Apply blocked"}</strong>
              {blockers.length > 0 && blockers.map((blocker) => <span key={blocker}>{blocker}</span>)}
              {props.applyResult.repos.map((repo) => (
                <span key={repo.repo_id}>
                  {repo.repo_name}: {repo.applied ? "applied" : repo.blocker ?? "no changes"}
                </span>
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
  onApply(limitPolicy: LimitPolicy, sandboxPolicy: SandboxPolicy, cloudPolicy: CloudPolicy): void;
  onOpenSettings(): void;
}) {
  const [limit, setLimit] = useState<LimitPolicy>(() => props.snapshot.limitPolicy ?? defaultLimitPolicy());
  const [sandbox, setSandbox] = useState<SandboxPolicy>(() => props.snapshot.sandboxPolicy ?? defaultSandboxPolicy());
  const [cloud, setCloud] = useState<CloudPolicy>(() => props.snapshot.cloudPolicy ?? defaultCloudPolicy());
  const claudeFirst = limit.agent_priority[0] !== "codex";
  const cloudBlockers = (props.snapshot.cloudAvailability ?? []).filter((item) => !item.ready);

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
            <div className="group-title">Readiness</div>
            <div className="readiness-grid">
              {props.snapshot.agents.map((agent) => (
                <div key={agent.kind} className="readiness-row">
                  <span>{labelAgent(agent.kind)}</span>
                  <small>{agent.installed ? (agent.authenticated ? humanize(agent.availability) : "Sign in needed") : "Not installed"}</small>
                </div>
              ))}
              {props.snapshot.localModels?.map((provider) => (
                <div key={provider.provider} className="readiness-row">
                  <span>{provider.label}</span>
                  <small>{provider.server_running ? `${provider.models.length} model${provider.models.length === 1 ? "" : "s"}` : "Offline"}</small>
                </div>
              ))}
              {props.snapshot.sandboxRuntime && (
                <div className="readiness-row">
                  <span>Docker Sandbox</span>
                  <small>{props.snapshot.sandboxRuntime.installed ? (props.snapshot.sandboxRuntime.authenticated ? "Ready" : "Sign in needed") : "Not installed"}</small>
                </div>
              )}
              {props.snapshot.cloudAvailability.map((item) => (
                <div key={item.agent} className="readiness-row">
                  <span>{labelAgent(item.agent)} cloud</span>
                  <small>{item.ready ? "Ready" : item.blockers[0] ?? "Not ready"}</small>
                </div>
              ))}
            </div>
          </div>

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
            <div className="group-title">Cloud continuity</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={cloud.enabled}
                onChange={(event) => setCloud({ ...cloud, enabled: event.target.checked })}
              />
              <span>Auto carryover tasks to the cloud</span>
            </label>
            {cloud.enabled && (
              <>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.continue_on_sleep}
                    onChange={(event) => setCloud({ ...cloud, continue_on_sleep: event.target.checked })}
                  />
                  <span>Carry over when the machine sleeps</span>
                </label>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.continue_on_shutdown}
                    onChange={(event) => setCloud({ ...cloud, continue_on_shutdown: event.target.checked })}
                  />
                  <span>Carry over on shutdown</span>
                </label>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.require_approval}
                    onChange={(event) => setCloud({ ...cloud, require_approval: event.target.checked })}
                  />
                  <span>Ask before handing work to the cloud</span>
                </label>
                <div className="field">
                  <span>Cloud provider</span>
                  <div className="segmented">
                    <button
                      type="button"
                      className={!cloud.allow_cross_provider ? "selected" : ""}
                      title="Claude Code tasks continue on Claude Code on the web; Codex tasks on Codex Cloud."
                      onClick={() => setCloud({ ...cloud, allow_cross_provider: false })}
                    >
                      Same provider
                    </button>
                    <button
                      type="button"
                      className={cloud.allow_cross_provider ? "selected" : ""}
                      title="Hand the task to the other provider's cloud when the current one isn't ready."
                      onClick={() => setCloud({ ...cloud, allow_cross_provider: true })}
                    >
                      Allow switching
                    </button>
                  </div>
                </div>
                <div className="field-grid">
                  <label className="field">
                    <span>Max cloud runs</span>
                    <input
                      type="number"
                      min={1}
                      max={8}
                      value={cloud.max_concurrent_cloud_runs}
                      onChange={(event) =>
                        setCloud({ ...cloud, max_concurrent_cloud_runs: Number(event.target.value) })
                      }
                    />
                  </label>
                  <label className="field">
                    <span>Codex environment ID</span>
                    <input
                      value={cloud.codex_env_id ?? ""}
                      placeholder="From chatgpt.com/codex"
                      onChange={(event) =>
                        setCloud({ ...cloud, codex_env_id: event.target.value.trim() || null })
                      }
                    />
                  </label>
                </div>
                {cloudBlockers.map((item) => (
                  <div key={item.agent} className="menu-empty">
                    {labelAgent(item.agent)} cloud: {item.blockers.length ? item.blockers.join(" ") : "not ready"}
                  </div>
                ))}
              </>
            )}
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
          <button type="button" className="primary" onClick={() => props.onApply(limit, sandbox, cloud)}>
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
  el.style.height = `${Math.min(el.scrollHeight, 180)}px`;
}

function runDefaults(snapshot: WorkbenchSnapshot, agent: AgentKind) {
  return snapshot.runDefaults.find((item) => item.kind === agent) ?? { model: null, reasoning: null };
}

type PickerModelOption = {
  value: string;
  label: string;
  source: string;
};

function modelOptions(
  agent: AgentKind,
  snapshot: WorkbenchSnapshot | null,
  localProvider: LocalModelProvider | null,
  current: string
): PickerModelOption[] {
  const out: PickerModelOption[] = [];
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  if (agent === "codex" && localProvider) {
    const status = snapshot?.localModels?.find((item) => item.provider === localProvider);
    for (const model of status?.models ?? []) {
      pushPickerOption(out, {
        value: model.id,
        label: model.name || prettyModel(model.id),
        source: `${status?.label ?? labelLocalProvider(localProvider)}${model.loaded ? " loaded" : ""}`,
      });
    }
  } else {
    for (const option of catalog?.models ?? []) {
      pushPickerOption(out, catalogOption(option));
    }
  }

  const defaults = snapshot ? runDefaults(snapshot, agent) : { model: null, reasoning: null };
  if (defaults.model?.trim()) {
    pushPickerOption(out, {
      value: defaults.model.trim(),
      label: prettyModel(defaults.model),
      source: "CLI default",
    });
  }
  if (current.trim()) {
    pushPickerOption(out, {
      value: current.trim(),
      label: prettyModel(current),
      source: out.some((option) => modelIdsEqual(option.value, current)) ? "Detected" : "Custom",
    });
  }
  return out;
}

function catalogOption(option: AgentModelOption): PickerModelOption {
  return {
    value: option.id,
    label: option.label || prettyModel(option.id),
    source: option.default ? `${sourceLabel(option.source)} default` : sourceLabel(option.source),
  };
}

function pushPickerOption(out: PickerModelOption[], option: PickerModelOption): void {
  if (!option.value.trim()) return;
  const existing = out.find((item) => modelIdsEqual(item.value, option.value));
  if (existing) {
    if (existing.source === "Custom") existing.source = option.source;
    return;
  }
  out.push(option);
}

function reasoningOptions(agent: AgentKind, snapshot: WorkbenchSnapshot | null): { value: string; label: string }[] {
  const values = [""];
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  for (const value of catalog?.reasoning ?? []) pushReasoning(values, value);
  const defaults = snapshot ? runDefaults(snapshot, agent) : { model: null, reasoning: null };
  if (defaults.reasoning) pushReasoning(values, defaults.reasoning);
  for (const fallback of agent === "claude_code" ? ["low", "medium", "high", "xhigh", "max"] : ["low", "medium", "high"]) {
    pushReasoning(values, fallback);
  }
  return values.map((value) => ({ value, label: value ? humanize(value) : "Default" }));
}

function pushReasoning(values: string[], value: string): void {
  const trimmed = value.trim();
  if (!trimmed) return;
  if (!values.some((item) => item.toLowerCase() === trimmed.toLowerCase())) values.push(trimmed);
}

function sourceLabel(source: string): string {
  switch (source) {
    case "codex_debug_models":
      return "Codex";
    case "claude_help":
      return "Claude";
    case "settings":
      return "Settings";
    default:
      return humanize(source);
  }
}

function modelIdsEqual(a: string, b: string): boolean {
  return baseModelId(a).toLowerCase() === baseModelId(b).toLowerCase();
}

// Strips provider decorations from a model id — e.g. the "[1m]" 1M-context
// suffix Claude Code appends — so display and dedupe work on the base id.
// The raw id (suffix included) is still what gets passed to the CLI.
function baseModelId(value: string): string {
  return value.trim().replace(/\[[^\]]*\]$/, "").trim();
}

// Display-only: turn a raw model id into a properly-capitalized name.
function prettyModel(value: string): string {
  const v = baseModelId(value);
  if (!v) return "";
  const known: Record<string, string> = {
    opus: "Opus",
    sonnet: "Sonnet",
    haiku: "Haiku",
    fable: "Fable",
    "claude-fable-5": "Claude Fable 5",
    "claude-opus-4-8": "Claude Opus 4.8",
    "claude-sonnet-5": "Claude Sonnet 5",
    "claude-sonnet-4-6": "Claude Sonnet 4.6",
    "claude-haiku-4-5": "Claude Haiku 4.5",
    "gpt-5": "GPT-5",
    "gpt-5.5": "GPT-5.5",
    "gpt-5-codex": "GPT-5 Codex",
    "gpt-4.1": "GPT-4.1",
    o3: "o3",
    "o4-mini": "o4-mini",
  };
  const hit = known[v.toLowerCase()];
  if (hit) return hit;
  // Claude ids follow "claude-<family>-<major>[-<minor>]": capitalize the words
  // and join trailing version numbers with dots (claude-opus-4-8 -> Claude Opus 4.8).
  if (/^claude[-_]/i.test(v)) {
    const words: string[] = [];
    const version: string[] = [];
    for (const part of v.split(/[-_]+/).filter(Boolean)) {
      if (/^\d+$/.test(part)) {
        version.push(part);
      } else {
        if (version.length) {
          words.push(version.join("."));
          version.length = 0;
        }
        words.push(part.charAt(0).toUpperCase() + part.slice(1));
      }
    }
    if (version.length) words.push(version.join("."));
    return words.join(" ");
  }
  return v.replace(/\bgpt\b/gi, "GPT").replace(/(^|[\s\-_])([a-z])/g, (_match, sep, ch) => sep + ch.toUpperCase());
}

function formatModelSwitch(from: string | null | undefined, to: string | null | undefined): string {
  const fromLabel = from ? prettyModel(from) : "default model";
  const toLabel = to ? prettyModel(to) : "default model";
  return `${fromLabel} -> ${toLabel}`;
}

function labelLocalProvider(provider: LocalModelProvider | ""): string {
  return provider === "lm_studio" ? "LM Studio" : provider === "ollama" ? "Ollama" : "Off";
}

function defaultLocalBaseUrl(provider: LocalModelProvider): string {
  return provider === "lm_studio" ? "http://127.0.0.1:1234" : "http://127.0.0.1:11434";
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

function isActivityEvent(event: AgentThreadEvent): boolean {
  return event.role === "tool" || event.role === "app" || event.role === "system";
}

function activityIcon(event: AgentThreadEvent) {
  if (event.kind === "file_changed") return "repo" as const;
  if (event.kind === "token_usage") return "clock" as const;
  if (event.kind === "usage_limit" || event.kind === "network_unavailable" || event.kind === "error") return "alert" as const;
  if (event.role === "tool") return "terminal" as const;
  return "agent" as const;
}

function activitySummary(event: AgentThreadEvent): string {
  const data = asRecord(event.data);
  if (event.kind === "tool_use") {
    const name = event.text ? humanize(event.text) : "Tool";
    const path = toolPath(data?.input);
    const command = toolCommand(data?.input);
    if (path) return `${name} ${path}`;
    if (command) return `${name} ${command}`;
    return name;
  }
  if (event.kind === "tool_result") {
    return event.text ? `Completed: ${event.text}` : "Tool completed";
  }
  if (event.kind === "file_changed") {
    return event.text ?? "File changed";
  }
  if (event.kind === "token_usage") {
    return event.text ?? "Token usage";
  }
  if (event.kind === "session_started") return "Session started";
  if (event.kind === "session_ended") return `Session ${String(event.text ?? "ended").toLowerCase()}`;
  if (event.kind === "usage_limit") return event.text ?? "Usage limit reached";
  if (event.kind === "network_unavailable") return event.text ?? "Network unavailable";
  if (event.kind === "awaiting_approval") return event.text ?? "Awaiting approval";
  if (event.kind === "error") return event.text ?? "Error";
  return event.text ?? humanize(event.kind);
}

function activityDetail(event: AgentThreadEvent): string | null {
  const data = asRecord(event.data);
  if (!data) return null;
  const detail =
    typeof data.summary === "string"
      ? data.summary
      : data.input !== undefined
        ? JSON.stringify(data.input, null, 2)
        : JSON.stringify(data, null, 2);
  if (!detail || detail === "{}") return null;
  return truncateDetail(detail, 1800);
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : null;
}

function toolPath(value: unknown): string | null {
  const input = asRecord(value);
  const candidate = input?.file_path ?? input?.path ?? input?.filepath ?? input?.filename;
  return typeof candidate === "string" && candidate.trim() ? candidate.trim() : null;
}

function toolCommand(value: unknown): string | null {
  const input = asRecord(value);
  const candidate = input?.command;
  if (typeof candidate === "string") return truncateDetail(candidate, 120);
  if (Array.isArray(candidate)) return truncateDetail(candidate.join(" "), 120);
  return null;
}

function truncateDetail(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, max)}\n\n[details truncated]`;
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

// Mirrors the daemon's built-in cloud policy defaults; only used before the
// first get_cloud_policy response arrives.
function defaultCloudPolicy(): CloudPolicy {
  return {
    enabled: false,
    continue_on_sleep: true,
    continue_on_shutdown: true,
    allow_cross_provider: false,
    checkpoint_interval_secs: 120,
    monitor_poll_secs: 30,
    stall_timeout_secs: 900,
    max_concurrent_cloud_runs: 2,
    codex_env_id: null,
    require_approval: false,
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
