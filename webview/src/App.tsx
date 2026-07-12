import {
  Fragment,
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { CSSProperties, ReactNode } from "react";
import type {
  AgentKind,
  AgentModelOption,
  AgentStatus,
  AgentThread,
  AgentThreadEvent,
  ApprovalDecision,
  ApprovalRequest,
  AvailabilityState,
  CloudPolicy,
  CloudRun,
  ExecutionBackend,
  ExtensionMessage,
  GithubRepository,
  LimitPolicy,
  LocalModelPolicy,
  LocalModelProvider,
  PermissionPolicy,
  SandboxPolicy,
  ThreadDetails,
  WorkbenchSnapshot,
} from "./types";
import { BrandMark, Icon } from "./icons";
import { Markdown } from "./markdown";
import {
  buildTranscriptItems,
  reconcilePendingMessages,
  type PendingTranscriptMessage,
  type TranscriptItem,
} from "./transcript";
import {
  formatQuestionAnswers,
  questionsFromEvent,
  type UserQuestion,
} from "./userQuestions";

type PendingMessage = PendingTranscriptMessage;
type PersistedState = {
  repoIds?: string[];
  repoTouched?: boolean;
};

type PendingRepoAssignment = {
  threadId: string;
  repoIds: string[];
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
  const [permission, setPermission] =
    useState<PermissionPolicy>("workspace_write");
  const [backend, setBackend] = useState<ExecutionBackend>("host");
  const [model, setModel] = useState("");
  const [reasoning, setReasoning] = useState("medium");
  const [localProvider, setLocalProvider] = useState<LocalModelProvider | "">(
    "",
  );
  const [localBaseUrl, setLocalBaseUrl] = useState("");
  const [repoIds, setRepoIds] = useState<string[]>(persisted.repoIds ?? []);
  const repoIdsRef = useRef(repoIds);
  repoIdsRef.current = repoIds;
  const [notice, setNotice] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [monitorOpen, setMonitorOpen] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [githubOpen, setGithubOpen] = useState(false);
  const [reviewOpen, setReviewOpen] = useState<{
    threadId: string;
    nonce: number;
  } | null>(null);
  const [githubRepos, setGithubRepos] = useState<GithubRepository[]>([]);
  const [githubLoading, setGithubLoading] = useState(false);
  const [welcomeLeaving, setWelcomeLeaving] = useState(false);
  // Optimistically-rendered user messages: shown the instant the user sends, then
  // dropped once the real event for them arrives in a snapshot.
  const [pending, setPending] = useState<PendingMessage[]>([]);
  const [editDraft, setEditDraft] = useState<{ text: string; nonce: number } | null>(null);
  const [answeredQuestionEvents, setAnsweredQuestionEvents] = useState<Set<string>>(
    () => new Set(),
  );
  // Optimistic thread navigation: reflect the clicked thread instantly instead of
  // waiting for the round-trip. `undefined` means "no navigation in flight".
  const [navThreadId, setNavThreadId] = useState<string | null | undefined>(
    undefined,
  );
  const transcriptRef = useRef<HTMLDivElement | null>(null);
  const snapshotRef = useRef<WorkbenchSnapshot | null>(null);
  const animatedMessageIdsRef = useRef(new Set<string>());
  const firstTurnMessageIdsRef = useRef(new Set<string>());
  const welcomeTimerRef = useRef<number | null>(null);
  const stickToBottomRef = useRef(true);
  const transcriptScrollStateRef = useRef<{
    threadId: string | null;
    pendingCount: number;
  }>({ threadId: null, pendingCount: 0 });
  const runControlsKeyRef = useRef<string>("");
  const handoffRef = useRef<Record<string, string>>({});
  // Repository choices are session UI state, not a reason to suppress the
  // current VS Code workspace defaults after the webview is reopened.
  const repoTouchedRef = useRef(false);
  const repoInitKeyRef = useRef("");
  const pendingRepoAssignmentRef = useRef<PendingRepoAssignment | null>(null);

  useEffect(() => {
    const onMessage = (event: MessageEvent<ExtensionMessage>) => {
      const incoming = event.data;
      if (incoming.type === "threadEvent") {
        const current = snapshotRef.current;
        const details = current?.details;
        if (
          !current ||
          !details ||
          current.selectedThreadId !== incoming.event.thread_id
        ) {
          return;
        }
        const events = [...details.events];
        const index = events.findIndex((item) => item.id === incoming.event.id);
        if (index >= 0) events[index] = incoming.event;
        else events.push(incoming.event);
        const next = {
          ...current,
          details: { ...details, events },
        };
        if (incoming.event.role === "assistant" && incoming.event.text) {
          animatedMessageIdsRef.current.add(incoming.event.id);
        }
        snapshotRef.current = next;
        setSnapshot(next);
        const selected = current.threads.find(
          (thread) => thread.id === current.selectedThreadId,
        );
        setPending((prev) =>
          reconcilePendingMessages({
            pending: prev,
            selectedStatus: selected?.status,
            events,
            queued: details.queued,
          }),
        );
        return;
      }
      if (incoming.type === "snapshot") {
        const previous = snapshotRef.current;
        const sameThread =
          !!previous &&
          previous.selectedThreadId === incoming.snapshot.selectedThreadId;
        if (!sameThread) {
          animatedMessageIdsRef.current.clear();
        } else {
          const previousEvents = new Map(
            (previous.details?.events ?? []).map((item) => [item.id, item]),
          );
          for (const item of incoming.snapshot.details?.events ?? []) {
            if (item.role !== "assistant" || !item.text) continue;
            const before = previousEvents.get(item.id);
            if (!before || before.text !== item.text) {
              animatedMessageIdsRef.current.add(item.id);
            }
          }
        }
        snapshotRef.current = incoming.snapshot;
        setSnapshot(incoming.snapshot);
        if (incoming.snapshot.error) setNotice(incoming.snapshot.error);
        // Clear the optimistic navigation once the daemon agrees on the selection.
        setNavThreadId((nav) =>
          nav === undefined || nav === incoming.snapshot.selectedThreadId
            ? undefined
            : nav,
        );
        // Drop optimistic bubbles once the real message lands — either as a user
        // event (sent immediately) or as a queued turn (sent while running).
        const events = incoming.snapshot.details?.events ?? [];
        const queued = incoming.snapshot.details?.queued ?? [];
        const selected = incoming.snapshot.threads.find(
          (thread) => thread.id === incoming.snapshot.selectedThreadId,
        );
        setPending((prev) =>
          reconcilePendingMessages({
            pending: prev,
            selectedStatus: selected?.status,
            events,
            queued,
          }),
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
        const next = repoIdsRef.current.includes(incoming.repo.id)
          ? repoIdsRef.current
          : [...repoIdsRef.current, incoming.repo.id];
        repoIdsRef.current = next;
        setRepoIds(next);
        const current = snapshotRef.current;
        const currentThreadId = current?.selectedThreadId;
        const reposLocked =
          !!currentThreadId &&
          !!current?.details?.repos.some((repo) => !!repo.worktree_path);
        if (currentThreadId && !reposLocked) {
          pendingRepoAssignmentRef.current = {
            threadId: currentThreadId,
            repoIds: [...next],
          };
          vscode.postMessage({
            type: "assignRepos",
            threadId: currentThreadId,
            repoIds: next,
          });
        }
        setNotice(`Connected ${incoming.repo.name}.`);
        return;
      }
      if (incoming.type === "repoAssignmentFailed") {
        if (
          pendingRepoAssignmentRef.current?.threadId === incoming.threadId
        ) {
          pendingRepoAssignmentRef.current = null;
          const current = snapshotRef.current;
          if (current?.selectedThreadId === incoming.threadId) {
            setRepoIds(
              current.details?.repos.map((repo) => repo.repo_id) ?? [],
            );
          }
        }
        setNotice(incoming.message);
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

  useEffect(
    () => () => {
      if (welcomeTimerRef.current !== null) {
        window.clearTimeout(welcomeTimerRef.current);
      }
    },
    [],
  );

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

  const effectiveSelectedId =
    navThreadId !== undefined
      ? navThreadId
      : (snapshot?.selectedThreadId ?? null);
  const selectedThread = useMemo(
    () =>
      snapshot?.threads.find((thread) => thread.id === effectiveSelectedId) ??
      null,
    [snapshot, effectiveSelectedId],
  );
  // Details belong to the daemon's current selection; while navigating to a
  // different thread they're stale, so we show a loading state instead.
  const navigating =
    navThreadId !== undefined &&
    navThreadId !== null &&
    navThreadId !== (snapshot?.selectedThreadId ?? null);
  useEffect(() => {
    if (!snapshot) return;
    const nextAgent =
      selectedThread?.active_agent ??
      selectedThread?.preferred_agent ??
      snapshot.defaults.agent;
    const defaults = runDefaults(snapshot, nextAgent);
    const nextPermission =
      selectedThread?.permission ?? snapshot.defaults.permission;
    const nextBackend = sanitizeBackend(
      nextAgent,
      selectedThread?.execution_backend ?? snapshot.defaults.execution_backend,
    );
    const nextModel =
      selectedThread?.model ?? snapshot.defaults.model ?? defaults.model ?? "";
    const nextReasoning =
      selectedThread?.reasoning ??
      snapshot.defaults.reasoning ??
      defaults.reasoning ??
      "medium";
    const nextLocalProvider =
      selectedThread?.local_provider ?? snapshot.defaults.local_provider ?? "";
    const nextLocalBaseUrl =
      selectedThread?.local_base_url ?? snapshot.defaults.local_base_url ?? "";
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
    if (
      selectedThread &&
      snapshot.selectedThreadId === selectedThread.id &&
      snapshot.details
    ) {
      const nextRepoIds = snapshot.details.repos.map((repo) => repo.repo_id);
      const pendingAssignment = pendingRepoAssignmentRef.current;
      if (pendingAssignment?.threadId === selectedThread.id) {
        if (!sameStringSet(pendingAssignment.repoIds, nextRepoIds)) return;
        pendingRepoAssignmentRef.current = null;
      }
      setRepoIds(nextRepoIds);
    } else if (!selectedThread) {
      const repoKey = [
        snapshot.repos.map((repo) => repo.id).join("|"),
        (snapshot.defaultRepoIds ?? []).join("|"),
      ].join("::");
      if (!repoTouchedRef.current && repoInitKeyRef.current !== repoKey) {
        repoInitKeyRef.current = repoKey;
        const nextRepoIds = snapshot.defaultRepoIds ?? [];
        repoIdsRef.current = nextRepoIds;
        setRepoIds(nextRepoIds);
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
    snapshot?.details?.repos,
    snapshot?.defaultRepoIds,
    snapshot?.runDefaults,
  ]);

  // Catalog refreshes may reveal that a persisted effort does not belong to
  // the selected model. Repair it immediately to the model's advertised
  // default instead of waiting for a provider-side launch error.
  useEffect(() => {
    const next = reasoningAfterModelChange(agent, snapshot, model, reasoning);
    if (next !== reasoning) setReasoning(next);
  }, [agent, model, reasoning, snapshot?.modelCatalog]);

  // A disconnected repo must not linger in the draft selection.
  useEffect(() => {
    if (!snapshot) return;
    const known = new Set(snapshot.repos.map((repo) => repo.id));
    const next = repoIdsRef.current.filter((id) => known.has(id));
    if (next.length === repoIdsRef.current.length) return;
    repoIdsRef.current = next;
    setRepoIds(next);
  }, [snapshot?.repos]);

  useEffect(() => {
    if (!selectedThread) return;
    const fallback = selectedThread.fallback_agent;
    const active = selectedThread.active_agent;
    const original = selectedThread.original_agent;
    const isFallbackActive =
      !!fallback &&
      active === fallback &&
      (selectedThread.handoff_state === "fallback_active" || !!original);
    const key = isFallbackActive
      ? `${selectedThread.id}:${original ?? "agent"}:${fallback}:${selectedThread.model ?? ""}`
      : `${selectedThread.id}:none`;
    if (handoffRef.current[selectedThread.id] === key) return;
    handoffRef.current[selectedThread.id] = key;
    if (!isFallbackActive) return;
    const modelNote =
      selectedThread.original_model !== selectedThread.model
        ? ` (${formatModelSwitch(selectedThread.original_model, selectedThread.model)})`
        : "";
    setNotice(
      `Rate Limit reached, switched to ${labelAgent(fallback)}${modelNote}.`,
    );
  }, [
    selectedThread?.id,
    selectedThread?.active_agent,
    selectedThread?.original_agent,
    selectedThread?.fallback_agent,
    selectedThread?.handoff_state,
    selectedThread?.model,
    selectedThread?.original_model,
  ]);

  const onTranscriptScroll = () => {
    const el = transcriptRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom < 96;
  };

  useEffect(() => {
    const threadId = selectedThread?.id ?? null;
    const previous = transcriptScrollStateRef.current;
    const threadChanged = previous.threadId !== threadId;
    const pendingAdded = pending.length > previous.pendingCount;
    transcriptScrollStateRef.current = {
      threadId,
      pendingCount: pending.length,
    };
    if (threadChanged) stickToBottomRef.current = true;
    if (!stickToBottomRef.current && !pendingAdded) return;
    window.requestAnimationFrame(() => {
      const el = transcriptRef.current;
      if (!el) return;
      el.scrollTo({
        top: el.scrollHeight,
        behavior:
          pendingAdded && !prefersReducedMotion() && !threadChanged
            ? "smooth"
            : "auto",
      });
    });
  }, [
    snapshot?.details?.events.length,
    snapshot?.details?.events.reduce(
      (length, event) => length + (event.text?.length ?? 0),
      0,
    ),
    snapshot?.details?.activities.length,
    snapshot?.details?.queued.length,
    snapshot?.details?.cloudRuns.length,
    selectedThread?.id,
    pending.length,
    reviewOpen?.nonce,
  ]);

  // Surface agent availability changes as a notice instead of an always-on badge.
  const availabilityRef = useRef<Record<string, AvailabilityState>>({});
  useEffect(() => {
    if (!snapshot) return;
    for (const status of snapshot.agents) {
      const previous = availabilityRef.current[status.kind];
      availabilityRef.current[status.kind] = status.availability;
      if (!previous || previous === status.availability) continue;
      if (status.availability === "limited") {
        const activeFallback =
          selectedThread?.fallback_agent &&
          selectedThread.active_agent === selectedThread.fallback_agent;
        if (activeFallback && status.kind === selectedThread?.original_agent)
          continue;
        const until = status.reset_at
          ? ` until ${formatResetTime(status.reset_at)}`
          : "";
        setNotice(`${labelAgent(status.kind)} is rate limited${until}.`);
      } else if (
        previous === "limited" &&
        status.availability === "available"
      ) {
        setNotice(`${labelAgent(status.kind)} is available again.`);
      }
    }
  }, [snapshot]);

  const isRunning = selectedThread?.status === "running";
  const details = navigating ? undefined : snapshot?.details;
  const reposLocked =
    !!selectedThread && !!details?.repos.some((repo) => !!repo.worktree_path);
  const canReviewChanges =
    !!selectedThread && !!details?.repos.some((repo) => !!repo.worktree_path);
  const activeCloudRuns =
    details?.cloudRuns.filter((run) => isActiveCloudRun(run.status)) ?? [];
  const transcriptItems = useMemo(
    () =>
      buildTranscriptItems({
        thread: selectedThread,
        events: details?.events ?? [],
        activities: details?.activities ?? [],
        queued: details?.queued ?? [],
        cloudRuns: details?.cloudRuns ?? [],
        pending,
      }),
    [
      selectedThread,
      details?.events,
      details?.activities,
      details?.queued,
      details?.cloudRuns,
      pending,
    ],
  );
  const visibleTranscriptItems = useMemo(() => {
    if (pending.length === 0) return transcriptItems;
    const pendingIds = new Set(pending.map((item) => item.id));
    return transcriptItems.filter(
      (item) =>
        item.type !== "event" ||
        item.event.role !== "user" ||
        !pendingIds.has(clientMessageIdForEvent(item.event) ?? ""),
    );
  }, [pending, transcriptItems]);

  // The composer owns its own draft text so typing never re-renders the
  // transcript; it hands us the final text here on submit.
  const send = (raw: string): boolean => {
    const text = raw.trim();
    if (!text) return false;
    const command = resolveAppCommand(text, agent);
    if (command?.kind === "unsupported") {
      setNotice(
        `/${command.name} is not supported in Perpetual. Choose a command from the picker.`,
      );
      return false;
    }
    if (command?.kind === "error") {
      setNotice(command.message);
      return false;
    }
    if (command?.kind === "local") {
      switch (command.action) {
        case "help":
          setNotice(
            `Commands: ${availableSlashCommands(agent)
              .map((item) => `/${item.name}`)
              .join(", ")}`,
          );
          return true;
        case "status": {
          const availability = snapshot?.agents.find(
            (item) => item.kind === agent,
          )?.availability;
          const selectedRepoCount =
            snapshot?.repos.filter((repo) => repoIds.includes(repo.id)).length ??
            0;
          setNotice(
            [
              labelAgent(agent),
              availability ? humanize(availability) : "Status unavailable",
              model.trim() ? prettyModel(model) : "Default model",
              reasoning.trim()
                ? `${humanize(reasoning)} effort`
                : "Default effort",
              permissionComposerLabel(permission),
              backend === "docker_sandbox" ? "Docker Sandbox" : "Host",
              `${selectedRepoCount} repo${selectedRepoCount === 1 ? "" : "s"}`,
              selectedThread ? (isRunning ? "Running" : "Idle") : "New session",
            ].join(" · "),
          );
          return true;
        }
        case "diff":
          if (!selectedThread) {
            setNotice("Start or resume a session before opening its changes.");
            return false;
          }
          reviewChanges();
          return true;
        case "new":
          newSession();
          return true;
        case "resume":
          setHistoryOpen(true);
          return true;
        case "settings":
          setSettingsOpen(true);
          return true;
        case "stop":
          if (!selectedThread || !isRunning) {
            setNotice("There is no active run to stop.");
            return false;
          }
          vscode.postMessage({
            type: "stopThread",
            threadId: selectedThread.id,
          });
          return true;
      }
    }
    if (command?.kind === "setting") {
      if (command.setting === "model") {
        const nextModel = command.argument.trim();
        if (!nextModel) {
          setNotice("Usage: /model <model-id>, or /model default.");
          return false;
        }
        if (nextModel.toLowerCase() === "default") {
          setModel("");
          setNotice(`Using the ${labelAgent(agent)} default model for future runs.`);
          return true;
        }
        setModel(nextModel);
        setReasoning(
          reasoningAfterModelChange(agent, snapshot, nextModel, reasoning),
        );
        setNotice(`Model set to ${prettyModel(nextModel)} for future ${labelAgent(agent)} runs.`);
        return true;
      }
      if (command.setting === "reasoning") {
        const requestedEffort = command.argument.trim();
        if (!requestedEffort) {
          setNotice("Usage: /effort <level>, or /effort default.");
          return false;
        }
        if (["auto", "default"].includes(requestedEffort.toLowerCase())) {
          setReasoning("");
          setNotice(`Using the ${labelAgent(agent)} default reasoning effort.`);
          return false;
        }
        const supportedEffort = reasoningOptions(agent, snapshot, model).find(
          (option) =>
            option.value.toLowerCase() === requestedEffort.toLowerCase(),
        );
        if (!supportedEffort?.value) {
          setNotice(
            `${humanize(requestedEffort)} reasoning is not available for ${labelAgent(agent)}.`,
          );
          return true;
        }
        setReasoning(supportedEffort.value);
        setNotice(
          `Reasoning effort set to ${humanize(supportedEffort.value)} for future ${labelAgent(agent)} runs.`,
        );
        return true;
      }

      const nextPermission = permissionFromCommand(command.argument);
      if (!nextPermission) {
        setNotice("Usage: /permissions read-only, write, or full-access.");
        return false;
      }
      setPermission(nextPermission);
      setNotice(`Permission set to ${permissionComposerLabel(nextPermission)} for future runs.`);
      return true;
    }
    if (!snapshot?.trusted) return false;
    const validRepoIds = snapshot.repos
      .filter((repo) => repoIds.includes(repo.id))
      .map((repo) => repo.id);
    if (snapshot.repos.length > 0 && validRepoIds.length === 0) {
      setNotice(
        "Select at least one connected repository before starting the agent.",
      );
      return false;
    }
    const run = command?.kind === "run" ? command : null;
    if (run?.requiresWrite && permission === "read_only") {
      setNotice(
        `/${text.split(/\s/, 1)[0].slice(1)} requires write access. Use /permissions write first.`,
      );
      return false;
    }
    const message = run?.message ?? text;
    const runPermission = run?.permission ?? permission;
    const submittedLocalProvider =
      agent === "codex" ? localProvider || null : null;
    const submittedModel = sanitizeModelForAgent(
      agent,
      model,
      submittedLocalProvider,
    );
    if (model.trim() && !submittedModel) {
      setModel("");
      setNotice(
        `${prettyModel(model)} is not available for ${labelAgent(agent)}. Using the agent default model.`,
      );
    }
    const clientMessageId = newClientMessageId();
    const firstTurn = !selectedThread;
    if (firstTurn) {
      firstTurnMessageIdsRef.current.add(clientMessageId);
      setWelcomeLeaving(true);
      if (welcomeTimerRef.current !== null) {
        window.clearTimeout(welcomeTimerRef.current);
      }
      welcomeTimerRef.current = window.setTimeout(() => {
        welcomeTimerRef.current = null;
        setWelcomeLeaving(false);
      }, 520);
      setNavThreadId(undefined);
    }
    setPending((prev) => [
      ...prev,
      { id: clientMessageId, text, firstTurn },
    ]);
    vscode.postMessage({
      type: "submit",
      message,
      clientMessageId,
      threadId: selectedThread?.id ?? null,
      repoIds: validRepoIds,
      agent,
      permission: runPermission,
      executionBackend: sanitizeBackend(agent, backend),
      model: submittedModel,
      reasoning: reasoning.trim() || null,
      localProvider: submittedLocalProvider,
      localBaseUrl: submittedLocalProvider ? localBaseUrl.trim() || null : null,
    });
    return true;
  };

  const pickAgent = (nextAgent: AgentKind) => {
    setAgent(nextAgent);
    if (nextAgent !== "codex") {
      setBackend("host");
      setLocalProvider("");
      setLocalBaseUrl("");
    }
    if (!snapshot) {
      setModel("");
      setReasoning("");
      return;
    }
    const defaults = runDefaults(snapshot, nextAgent);
    setModel(defaults.model ?? "");
    setReasoning(defaults.reasoning ?? "");
  };

  const setDraftRepoIds = (next: string[]) => {
    repoTouchedRef.current = true;
    setRepoIds(next);
    if (selectedThread && !reposLocked) {
      pendingRepoAssignmentRef.current = {
        threadId: selectedThread.id,
        repoIds: [...next],
      };
      vscode.postMessage({
        type: "assignRepos",
        threadId: selectedThread.id,
        repoIds: next,
      });
    }
  };

  const newSession = () => {
    pendingRepoAssignmentRef.current = null;
    repoTouchedRef.current = false;
    repoInitKeyRef.current = "";
    const defaultRepoIds = snapshot?.defaultRepoIds ?? [];
    repoIdsRef.current = defaultRepoIds;
    setRepoIds(defaultRepoIds);
    setHistoryOpen(false);
    setPending([]);
    setWelcomeLeaving(false);
    animatedMessageIdsRef.current.clear();
    firstTurnMessageIdsRef.current.clear();
    if (welcomeTimerRef.current !== null) {
      window.clearTimeout(welcomeTimerRef.current);
      welcomeTimerRef.current = null;
    }
    // Reflect the empty composer instantly instead of waiting for the round-trip.
    setNavThreadId(null);
    vscode.postMessage({ type: "newSession" });
  };

  const selectThread = (id: string) => {
    setHistoryOpen(false);
    setPending([]);
    setWelcomeLeaving(false);
    animatedMessageIdsRef.current.clear();
    firstTurnMessageIdsRef.current.clear();
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
  const reviewChanges = () => {
    if (!selectedThread) return;
    setReviewOpen((prev) => ({
      threadId: selectedThread.id,
      nonce: (prev?.nonce ?? 0) + 1,
    }));
    vscode.postMessage({ type: "loadDiff", threadId: selectedThread.id });
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
          <IconButton title="Settings" onClick={() => setSettingsOpen(true)}>
            <Icon name="settings" />
          </IconButton>
          <IconButton title="Status monitor" onClick={() => setMonitorOpen(true)}>
            <Icon name="clock" />
          </IconButton>
          {canReviewChanges && (
            <IconButton title="Review changes" onClick={reviewChanges}>
              <Icon name="repo" />
            </IconButton>
          )}
          <IconButton
            title="Refresh"
            onClick={() => vscode.postMessage({ type: "refresh" })}
          >
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

      <CloudStatusBar
        selectedThread={selectedThread}
        activeRuns={activeCloudRuns}
        policy={snapshot?.cloudPolicy ?? null}
        availability={snapshot?.cloudAvailability ?? []}
        onLaunch={() =>
          selectedThread &&
          vscode.postMessage({
            type: "launchCloudHandoff",
            threadId: selectedThread.id,
            agent: selectedThread.active_agent ?? selectedThread.preferred_agent,
          })
        }
        onReclaim={() =>
          selectedThread &&
          vscode.postMessage({
            type: "reclaimCloudRun",
            threadId: selectedThread.id,
          })
        }
      />

      <section className="conversation">
        <div
          className={`transcript${welcomeLeaving ? " is-starting" : ""}`}
          ref={transcriptRef}
          onScroll={onTranscriptScroll}
        >
          {navigating && <div className="thinking">Loading…</div>}
          {!navigating &&
            ((!selectedThread && pending.length === 0) || welcomeLeaving) && (
            <EmptyState
              exiting={welcomeLeaving}
            />
          )}
          {(selectedThread || pending.length > 0) &&
            visibleTranscriptItems.map((item) => (
              <TranscriptItemView
                key={transcriptItemKey(item)}
                item={item}
                animateMessage={
                  item.type === "event" &&
                  animatedMessageIdsRef.current.has(item.event.id)
                }
                firstTurn={
                  item.type === "event" &&
                  firstTurnMessageIdsRef.current.has(
                    clientMessageIdForEvent(item.event) ?? "",
                  )
                }
                onEdit={
                  item.type === "event" && item.event.role === "user"
                    ? () =>
                        {
                          setEditDraft({
                            text: item.event.text ?? "",
                            nonce: Date.now(),
                          });
                          setNotice(
                            "Edit the message and send it as a new follow-up. Existing history stays intact.",
                          );
                        }
                    : undefined
                }
                questionAnswered={
                  item.type === "event" && answeredQuestionEvents.has(item.event.id)
                }
                onAnswerQuestions={(eventId, questions, answers) => {
                  if (!send(formatQuestionAnswers(questions, answers))) return;
                  setAnsweredQuestionEvents((current) => {
                    const next = new Set(current);
                    next.add(eventId);
                    return next;
                  });
                }}
              />
            ))}
          {details?.approvals.map((approval) => (
            <ApprovalCard
              key={approval.id}
              approval={approval}
              onResolve={(decision) =>
                vscode.postMessage({
                  type: "resolveApproval",
                  id: approval.id,
                  decision,
                })
              }
            />
          ))}
          {!navigating &&
            (selectedThread || pending.length > 0) &&
            (isRunning || pending.length > 0) &&
            !details?.approvals.length && (
              <WorkingIndicator />
            )}
          {!navigating &&
            selectedThread &&
            details &&
            details.events.length === 0 &&
            pending.length === 0 &&
            !isRunning && (
              <EmptyState compact />
            )}
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
        editDraft={editDraft}
        onEditDraftConsumed={() => setEditDraft(null)}
        onStop={() =>
          selectedThread &&
          vscode.postMessage({
            type: "stopThread",
            threadId: selectedThread.id,
          })
        }
        onGithub={() => {
          setGithubLoading(true);
          vscode.postMessage({ type: "githubList" });
        }}
        onLocalRepo={() => vscode.postMessage({ type: "connectLocalRepo" })}
        onRemoveRepo={(repoId) =>
          vscode.postMessage({ type: "deleteRepo", repoId })
        }
        onClearRepos={() => vscode.postMessage({ type: "clearRepos" })}
        onSandboxLogin={(codex) =>
          vscode.postMessage({ type: "sandboxLogin", codex })
        }
        onNewSession={newSession}
        onRefresh={() => vscode.postMessage({ type: "refresh" })}
        onReviewChanges={reviewChanges}
        onOpenSettings={() => setSettingsOpen(true)}
        onNotice={setNotice}
      />

      {selectedThread && details && (
        <ChangesView
          threadId={selectedThread.id}
          diff={details.diff}
          diffState={details.diffState ?? "idle"}
          repos={details.repos}
          applyResult={details.applyResult ?? null}
          openSignal={
            reviewOpen?.threadId === selectedThread.id ? reviewOpen.nonce : 0
          }
          onLoadDiff={(threadId) =>
            vscode.postMessage({ type: "loadDiff", threadId })
          }
          onApply={(threadId) =>
            vscode.postMessage({ type: "applyThreadChanges", threadId })
          }
          onOpenPath={(target) =>
            vscode.postMessage({ type: "openPath", path: target })
          }
        />
      )}

      {settingsOpen && snapshot && (
        <SettingsSheet
          snapshot={snapshot}
          onClose={() => setSettingsOpen(false)}
          onApply={(
            limitPolicy,
            sandboxPolicy,
            cloudPolicy,
            localModelPolicy,
          ) => {
            vscode.postMessage({ type: "setLimitPolicy", policy: limitPolicy });
            vscode.postMessage({
              type: "setSandboxPolicy",
              policy: sandboxPolicy,
            });
            vscode.postMessage({ type: "setCloudPolicy", policy: cloudPolicy });
            vscode.postMessage({
              type: "setLocalModelPolicy",
              policy: localModelPolicy,
            });
            setSettingsOpen(false);
          }}
          onOpenSettings={() => vscode.postMessage({ type: "openSettings" })}
          onOpenExternal={(url) =>
            vscode.postMessage({ type: "openExternal", url })
          }
          onSignInAgent={(agent) =>
            vscode.postMessage({ type: "signInAgent", agent })
          }
          onSandboxLogin={(codex) =>
            vscode.postMessage({ type: "sandboxLogin", codex })
          }
          onGithubSignIn={() => vscode.postMessage({ type: "githubSignIn" })}
          onRefreshReadiness={() =>
            vscode.postMessage({ type: "refreshReadiness" })
          }
        />
      )}

      {monitorOpen && snapshot && (
        <MonitorSheet
          snapshot={snapshot}
          selectedThread={selectedThread}
          details={details ?? null}
          onClose={() => setMonitorOpen(false)}
          onOpenSettings={() => {
            setMonitorOpen(false);
            setSettingsOpen(true);
          }}
          onLaunchCloud={() =>
            selectedThread &&
            vscode.postMessage({
              type: "launchCloudHandoff",
              threadId: selectedThread.id,
              agent: selectedThread.active_agent ?? selectedThread.preferred_agent,
            })
          }
          onReclaimCloud={() =>
            selectedThread &&
            vscode.postMessage({
              type: "reclaimCloudRun",
              threadId: selectedThread.id,
            })
          }
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

function TranscriptItemView({
  item,
  animateMessage = false,
  firstTurn = false,
  onEdit,
  questionAnswered = false,
  onAnswerQuestions,
}: {
  item: TranscriptItem;
  animateMessage?: boolean;
  firstTurn?: boolean;
  onEdit?: () => void;
  questionAnswered?: boolean;
  onAnswerQuestions?: (
    eventId: string,
    questions: UserQuestion[],
    answers: Record<string, string[]>,
  ) => void;
}) {
  if (item.type === "event") {
    return (
      <MessageView
        event={item.event}
        animate={animateMessage}
        firstTurn={firstTurn}
        onEdit={onEdit}
        questionAnswered={questionAnswered}
        onAnswerQuestions={onAnswerQuestions}
      />
    );
  }
  if (item.type === "queued") {
    return (
      <article className="activity-row queued-row">
        <span className="activity-icon">
          <Icon name="queue" />
        </span>
        <div className="activity-main">
          <div className="activity-title">
            <span>Queued: {item.message}</span>
          </div>
        </div>
      </article>
    );
  }
  if (item.type === "pending") {
    return (
      <article className={`msg user pending${item.firstTurn ? " first-turn" : ""}`}>
        <div className="msg-body">{item.text}</div>
      </article>
    );
  }
  return (
    <div className={`transition-row ${item.tone}`} role="status">
      <span className="transition-body">
        <Icon
          name={
            item.icon ??
            (item.tone === "danger"
              ? "alert"
              : item.tone === "warning"
                ? "clock"
                : "refresh")
          }
        />
        <span className="transition-text">{item.text}</span>
        {item.detail && <span className="transition-detail">{item.detail}</span>}
      </span>
    </div>
  );
}

function transcriptItemKey(item: TranscriptItem): string {
  if (item.type === "event") return item.event.id;
  return item.id;
}

function newClientMessageId(): string {
  const random =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  return `cm-${random}`;
}

function clientMessageIdForEvent(event: AgentThreadEvent): string | null {
  if (event.client_message_id?.trim()) return event.client_message_id.trim();
  const data = asRecord(event.data) ?? {};
  const value = data.client_message_id;
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function prefersReducedMotion(): boolean {
  return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
}

// One transcript row. Memoized so a snapshot tick only re-renders the messages
// that actually changed — an unchanged message keeps its parsed Markdown.
const MessageView = memo(function MessageView({
  event,
  animate = false,
  firstTurn = false,
  onEdit,
  questionAnswered = false,
  onAnswerQuestions,
}: {
  event: AgentThreadEvent;
  animate?: boolean;
  firstTurn?: boolean;
  onEdit?: () => void;
  questionAnswered?: boolean;
  onAnswerQuestions?: (
    eventId: string,
    questions: UserQuestion[],
    answers: Record<string, string[]>,
  ) => void;
}) {
  const questions = questionsFromEvent(event);
  if (questions.length > 0) {
    return (
      <UserQuestionCard
        eventId={event.id}
        questions={questions}
        answered={questionAnswered}
        onSubmit={onAnswerQuestions}
      />
    );
  }
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
  const streaming =
    event.role === "assistant" && (asRecord(event.data) ?? {}).streaming === true;
  return (
    <article
      className={`msg ${messageClass(event.role)}${firstTurn ? " first-turn-settled" : ""}`}
    >
      <div className="msg-body">
        {event.text ? (
          event.role === "assistant" ? (
            <StreamingMarkdown
              text={event.text}
              animate={animate}
              active={streaming}
            />
          ) : (
            <Markdown text={event.text} />
          )
        ) : (
          humanize(event.kind)
        )}
      </div>
      {event.role === "user" && onEdit && (
        <button
          type="button"
          className="message-edit-btn"
          title="Edit and resend as a new turn"
          aria-label="Edit and resend message"
          onClick={onEdit}
        >
          Edit
        </button>
      )}
    </article>
  );
});

function UserQuestionCard(props: {
  eventId: string;
  questions: UserQuestion[];
  answered: boolean;
  onSubmit?: (
    eventId: string,
    questions: UserQuestion[],
    answers: Record<string, string[]>,
  ) => void;
}) {
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [custom, setCustom] = useState<Record<string, string>>({});
  const complete = props.questions.every(
    (question) =>
      (selected[question.id]?.length ?? 0) > 0 || !!custom[question.id]?.trim(),
  );
  const choose = (question: UserQuestion, label: string) => {
    setCustom((current) => ({ ...current, [question.id]: "" }));
    setSelected((current) => {
      const existing = current[question.id] ?? [];
      const values = question.multiSelect
        ? existing.includes(label)
          ? existing.filter((item) => item !== label)
          : [...existing, label]
        : [label];
      return { ...current, [question.id]: values };
    });
  };
  return (
    <article className={`user-question-card${props.answered ? " answered" : ""}`}>
      {props.questions.map((question) => (
        <section key={question.id} className="user-question-section">
          <div className="user-question-header">{question.header}</div>
          <div className="user-question-text">{question.question}</div>
          <div className="user-question-options">
            {question.options.map((option) => {
              const active = selected[question.id]?.includes(option.label) ?? false;
              return (
                <button
                  key={option.label}
                  type="button"
                  className={`user-question-option${active ? " active" : ""}`}
                  aria-pressed={active}
                  disabled={props.answered}
                  onClick={() => choose(question, option.label)}
                >
                  <strong>{option.label}</strong>
                  {option.description && <small>{option.description}</small>}
                </button>
              );
            })}
          </div>
          <label className="user-question-other">
            <span>No — tell the agent what to do differently</span>
            <textarea
              rows={2}
              disabled={props.answered}
              value={custom[question.id] ?? ""}
              placeholder="Type your own response"
              onChange={(event) => {
                const value = event.target.value;
                setCustom((current) => ({ ...current, [question.id]: value }));
                if (value.trim()) {
                  setSelected((current) => ({ ...current, [question.id]: [] }));
                }
              }}
            />
          </label>
        </section>
      ))}
      <button
        type="button"
        className="primary-btn user-question-submit"
        disabled={!complete || props.answered || !props.onSubmit}
        onClick={() => {
          const answers = Object.fromEntries(
            props.questions.map((question) => [
              question.id,
              custom[question.id]?.trim()
                ? [custom[question.id].trim()]
                : selected[question.id] ?? [],
            ]),
          );
          props.onSubmit?.(props.eventId, props.questions, answers);
        }}
      >
        {props.answered ? "Response sent" : "Continue"}
      </button>
    </article>
  );
}

function StreamingMarkdown({
  text,
  animate,
  active,
}: {
  text: string;
  animate: boolean;
  active: boolean;
}) {
  const [visible, setVisible] = useState(() => (animate ? "" : text));
  const visibleRef = useRef(visible);

  useEffect(() => {
    if (!animate || !text.startsWith(visibleRef.current)) {
      visibleRef.current = text;
      setVisible(text);
      return;
    }
    let frame = 0;
    let lastPaint = performance.now();
    const reveal = (now: number) => {
      const elapsed = Math.min(50, now - lastPaint);
      lastPaint = now;
      const current = visibleRef.current.length;
      const backlog = text.length - current;
      if (backlog <= 0) return;

      // Keep the reveal moving at a brisk baseline, then accelerate in
      // proportion to the backlog so a burst of tokens never leaves the UI
      // visibly behind. Updating on every animation frame avoids the chunky
      // word-at-a-time cadence of the previous throttled loop.
      const charactersPerSecond = Math.min(1_400, 240 + backlog * 7);
      const chunk = Math.min(
        42,
        Math.max(2, Math.ceil((charactersPerSecond * elapsed) / 1_000)),
      );
      const end = nextRevealBoundary(text, current + chunk);
      const next = text.slice(0, end);
      visibleRef.current = next;
      setVisible(next);
      if (end < text.length) frame = window.requestAnimationFrame(reveal);
    };
    frame = window.requestAnimationFrame(reveal);
    return () => window.cancelAnimationFrame(frame);
  }, [animate, text]);

  const revealing = visible.length < text.length;
  return (
    <div className={`stream-content${active || revealing ? " is-streaming" : ""}`}>
      <Markdown text={visible} />
    </div>
  );
}

function nextRevealBoundary(text: string, proposed: number): number {
  let end = Math.min(text.length, Math.max(1, proposed));
  const previous = text.charCodeAt(end - 1);
  if (previous >= 0xd800 && previous <= 0xdbff && end < text.length) end += 1;
  return end;
}

function WorkingIndicator() {
  return (
    <div
      className="thinking working-indicator"
      role="status"
      aria-live="polite"
    >
      <span className="thinking-label">Working</span>
    </div>
  );
}

function IconButton(props: {
  title: string;
  onClick(): void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      className="icon-btn"
      type="button"
      title={props.title}
      aria-label={props.title}
      disabled={props.disabled}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

/** Generic viewport-anchored popover used by toolbar menus. */
function Popover(props: {
  trigger: (args: {
    open: boolean;
    toggle(): void;
    ref: (el: HTMLElement | null) => void;
  }) => ReactNode;
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
      const openUp =
        placement === "above" ||
        (placement === "auto" && spaceBelow < 220 && spaceAbove > spaceBelow);
      const availableHeight = openUp ? spaceAbove : spaceBelow;
      const maxHeight = Math.max(120, Math.min(360, availableHeight));
      const maxWidth = vw - margin * 2;
      const menuWidth = Math.min(
        menuRef.current?.offsetWidth ?? rect.width,
        maxWidth,
      );
      const menuHeight = Math.min(
        menuRef.current?.offsetHeight ?? maxHeight,
        maxHeight,
      );
      // Anchor by preferred edge, then clamp fully inside the viewport so menus never clip.
      const preferredLeft =
        align === "center"
          ? rect.left + rect.width / 2 - menuWidth / 2
          : align === "right"
            ? rect.right - menuWidth
            : rect.left;
      const left = Math.max(
        margin,
        Math.min(preferredLeft, vw - menuWidth - margin),
      );
      const preferredTop = openUp
        ? rect.top - menuHeight - margin
        : rect.bottom + margin;
      const top = Math.max(
        margin,
        Math.min(preferredTop, vh - menuHeight - margin),
      );
      const originX =
        rect.left + rect.width / 2 < left + menuWidth / 3
          ? "left"
          : rect.left + rect.width / 2 > left + (menuWidth * 2) / 3
            ? "right"
            : "center";
      const next: CSSProperties = {
        position: "fixed",
        maxHeight,
        maxWidth,
        minWidth: Math.min(rect.width, maxWidth),
        left,
        top,
        transformOrigin: `${originX} ${openUp ? "bottom" : "top"}`,
      };
      setMenuStyle(next);
    };
    reposition();
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    // Menus whose content swaps (drilling into the model list) change height
    // after they are placed, so re-anchor instead of letting them grow off-screen.
    const observer = new ResizeObserver(() => reposition());
    if (menuRef.current) observer.observe(menuRef.current);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [props.open, align, placement]);

  useEffect(() => {
    if (!props.open) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (
        !menuRef.current?.contains(target) &&
        !triggerRef.current?.contains(target)
      ) {
        props.setOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        props.setOpen(false);
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const focusable = Array.from(
        menuRef.current?.querySelectorAll<HTMLElement>(
          'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      ).filter((el) => el.offsetParent !== null);
      if (focusable.length === 0) return;
      event.preventDefault();
      const active = document.activeElement as HTMLElement | null;
      const current = active ? focusable.indexOf(active) : -1;
      const direction = event.key === "ArrowDown" ? 1 : -1;
      const next =
        current < 0
          ? event.key === "ArrowDown"
            ? 0
            : focusable.length - 1
          : (current + direction + focusable.length) % focusable.length;
      focusable[next]?.focus();
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
          style={
            menuStyle ?? {
              position: "fixed",
              top: 0,
              left: 0,
              visibility: "hidden",
              pointerEvents: "none",
            }
          }
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
            className={
              option.value === props.value ? "menu-item selected" : "menu-item"
            }
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

function ModelField(props: {
  agent: AgentKind;
  snapshot: WorkbenchSnapshot | null;
  localProvider: LocalModelProvider | null;
  value: string;
  onOpen(): void;
}) {
  const options = useMemo(
    () =>
      modelOptions(
        props.agent,
        props.snapshot,
        props.localProvider,
        props.value,
      ),
    [props.agent, props.snapshot, props.localProvider, props.value],
  );
  const selected = options.find((option) =>
    modelIdsEqual(option.value, props.value),
  );
  return (
    <div className="field">
      <span>Model</span>
      <button
        type="button"
        className="model-trigger"
        aria-haspopup="listbox"
        onClick={props.onOpen}
      >
        <span className="model-trigger-main">
          <span>
            {selected?.label ??
              (props.value ? prettyModel(props.value) : "Default model")}
          </span>
          <small>{selected?.source ?? "installed CLI default"}</small>
        </span>
        <Icon name="caret" />
      </button>
    </div>
  );
}

/*
 * The model list is a drill-in, not a disclosure inside the run options: nesting
 * a scrolling list inside the scrolling popover left it narrow and double-scrolled.
 * Taking over the whole popover gives the list the full width and a single scroller.
 */
function ModelBrowser(props: {
  agent: AgentKind;
  snapshot: WorkbenchSnapshot | null;
  localProvider: LocalModelProvider | null;
  value: string;
  onSelect(value: string): void;
  onBack(): void;
}) {
  const [query, setQuery] = useState("");
  const options = useMemo(
    () =>
      modelOptions(
        props.agent,
        props.snapshot,
        props.localProvider,
        props.value,
      ),
    [props.agent, props.snapshot, props.localProvider, props.value],
  );
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = normalizedQuery
    ? options.filter(
        (option) =>
          option.value.toLowerCase().includes(normalizedQuery) ||
          option.label.toLowerCase().includes(normalizedQuery) ||
          option.source.toLowerCase().includes(normalizedQuery),
      )
    : options;
  const custom = query.trim();
  const canUseCustom =
    !!custom && !options.some((option) => modelIdsEqual(option.value, custom));
  const groups: { source: string; options: PickerModelOption[] }[] = [];
  for (const option of filtered) {
    const group = groups.find((entry) => entry.source === option.source);
    if (group) group.options.push(option);
    else groups.push({ source: option.source, options: [option] });
  }

  return (
    <div className="model-browser">
      <div className="model-browser-head">
        <button
          type="button"
          className="model-back"
          aria-label="Back to run options"
          onClick={props.onBack}
        >
          <Icon name="caret" />
        </button>
        <strong>Model</strong>
      </div>
      <input
        className="model-search"
        autoFocus
        value={query}
        placeholder="Search or type a model id"
        onChange={(event) => setQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && custom) props.onSelect(custom);
        }}
      />
      <div className="model-list" role="listbox">
        {canUseCustom && (
          <button
            type="button"
            className="menu-item"
            onClick={() => props.onSelect(custom)}
          >
            <Icon name="plus" />
            <span className="history-text">
              <span>{custom}</span>
              <small>Use custom model id</small>
            </span>
          </button>
        )}
        {!normalizedQuery && (
          <button
            type="button"
            role="option"
            aria-selected={!props.value.trim()}
            className={!props.value.trim() ? "menu-item selected" : "menu-item"}
            onClick={() => props.onSelect("")}
          >
            <span className="history-text">
              <span>Default model</span>
              <small>Use the installed CLI default</small>
            </span>
            {!props.value.trim() && <Icon name="check" />}
          </button>
        )}
        {groups.map((group) => (
          <Fragment key={group.source}>
            <div className="menu-head">{group.source}</div>
            {group.options.map((option) => {
              const active = modelIdsEqual(option.value, props.value);
              return (
                <button
                  key={`${group.source}:${option.value}`}
                  type="button"
                  role="option"
                  aria-selected={active}
                  className={active ? "menu-item selected" : "menu-item"}
                  onClick={() => props.onSelect(option.value)}
                >
                  <span className="history-text">
                    <span>{option.label}</span>
                    <small>{option.value}</small>
                  </span>
                  {active && <Icon name="check" />}
                </button>
              );
            })}
          </Fragment>
        ))}
        {filtered.length === 0 && !canUseCustom && (
          <div className="menu-empty">No matching models</div>
        )}
      </div>
    </div>
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
          <button
            type="button"
            className="history-new"
            title="New session"
            aria-label="New session"
            onClick={props.onNew}
          >
            <Icon name="plus" />
          </button>
        </div>
        {threads.length === 0 && (
          <div className="menu-empty">No sessions yet</div>
        )}
        {threads.map((thread) => {
          const running = thread.status === "running";
          const selected = thread.id === props.selectedThread?.id;
          return (
            <div
              key={thread.id}
              className={selected ? "history-row selected" : "history-row"}
            >
              <button
                type="button"
                className="history-pick"
                onClick={() => props.onSelect(thread.id)}
                title={thread.title}
              >
                <span
                  className={running ? "history-spinner" : "history-dot"}
                  data-status={thread.status}
                />
                <span className="history-text">
                  <span className="history-title">{thread.title}</span>
                  <small>
                    {labelAgent(thread.active_agent ?? thread.preferred_agent)}{" "}
                    · {humanize(thread.status)}
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

const APPROVAL_ICON: Record<
  ApprovalRequest["kind"],
  "terminal" | "repo" | "agent"
> = {
  command: "terminal",
  file_change: "repo",
  tool: "agent",
};

function CloudStatusBar(props: {
  selectedThread: AgentThread | null;
  activeRuns: NonNullable<WorkbenchSnapshot["details"]>["cloudRuns"];
  policy: CloudPolicy | null;
  availability: WorkbenchSnapshot["cloudAvailability"];
  onLaunch(): void;
  onReclaim(): void;
}) {
  const agent =
    props.selectedThread?.active_agent ?? props.selectedThread?.preferred_agent ?? null;
  const active = props.activeRuns[0] ?? null;
  const ready = !!agent && props.availability.some((item) => item.agent === agent && item.ready);
  const canLaunch =
    !!props.selectedThread &&
    !!props.policy?.enabled &&
    !active &&
    ready &&
    props.selectedThread.status !== "draft";
  if (!active && !canLaunch && !props.policy?.enabled) return null;
  return (
    <div className="cloud-bar" role="status">
      {active ? (
        <>
          <span className="cloud-pill">
            <Icon name={active.status === "stalled" ? "alert" : "cloud"} />
            <span>{labelAgent(active.agent_kind)} Cloud</span>
            <strong>{humanize(active.status)}</strong>
          </span>
          <button type="button" className="cloud-action" onClick={props.onReclaim}>
            <Icon name="download" />
            <span>Reclaim</span>
          </button>
        </>
      ) : (
        <button
          type="button"
          className="cloud-action"
          disabled={!canLaunch}
          onClick={props.onLaunch}
          title={
            canLaunch
              ? "Continue this session in the provider cloud"
              : "Cloud continuation is not ready for this session"
          }
        >
          <Icon name="cloud" />
          <span>Cloud</span>
        </button>
      )}
    </div>
  );
}

function ApprovalCard(props: {
  approval: ApprovalRequest;
  onResolve(decision: ApprovalDecision): void;
}) {
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
      {commandText && (
        <pre className="approval-cmd">
          <code>{commandText}</code>
        </pre>
      )}
      {approval.cwd && <div className="approval-meta">in {approval.cwd}</div>}
      {approval.reason && (
        <div className="approval-reason">{approval.reason}</div>
      )}
      <div className="approval-actions">
        <button
          type="button"
          className="approval-btn allow"
          title="Allow once"
          onClick={() => props.onResolve("allow")}
        >
          <Icon name="check" />
          <span>Allow</span>
        </button>
        <button
          type="button"
          className="approval-btn deny"
          title="Deny this request"
          onClick={() => props.onResolve("deny")}
        >
          <Icon name="close" />
          <span>Deny</span>
        </button>
      </div>
    </article>
  );
}

type ComposerProps = {
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
  onSend(text: string): boolean;
  editDraft: { text: string; nonce: number } | null;
  onEditDraftConsumed(): void;
  onStop(): void;
  onGithub(): void;
  onLocalRepo(): void;
  onRemoveRepo(repoId: string): void;
  onClearRepos(): void;
  onSandboxLogin(codex: boolean): void;
  onNewSession(): void;
  onRefresh(): void;
  onReviewChanges(): void;
  onOpenSettings(): void;
  onNotice(message: string): void;
};

function Composer(props: ComposerProps) {
  const [reposOpen, setReposOpen] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const [permOpen, setPermOpen] = useState(false);
  // The draft lives here, not in App, so each keystroke re-renders only the
  // composer — never the transcript. App receives the text only on submit.
  const [draft, setDraft] = useState("");
  const [selectionStart, setSelectionStart] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    if (!props.editDraft) return;
    setDraft(props.editDraft.text);
    setSelectionStart(props.editDraft.text.length);
    props.onEditDraftConsumed();
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
      autoGrow(el);
    });
  }, [props.editDraft?.nonce]);
  const localOn = !!props.localProvider;
  const localAllowed = props.agent === "codex";
  const sandboxAllowed = props.agent === "codex";
  const sandboxOn = props.backend === "docker_sandbox";
  const sandbox = props.snapshot?.sandboxRuntime;
  const repos = props.snapshot?.repos ?? [];
  const selectedRepos = repos.filter((repo) => props.repoIds.includes(repo.id));
  const noRepoSelected = repos.length > 0 && selectedRepos.length === 0;
  const draftCommand = resolveAppCommand(draft, props.agent);
  const isAppOnlyCommand =
    draftCommand !== null && draftCommand.kind !== "run";
  const canSend =
    !!draft.trim() &&
    (isAppOnlyCommand ||
      (!!props.snapshot?.trusted &&
        !noRepoSelected &&
        (!localOn || !!props.model.trim())));
  const sendDisabledReason = !props.snapshot
    ? "Connecting to Perpetual"
    : !props.snapshot.trusted
      ? "Trust this workspace in VS Code to send messages"
      : noRepoSelected
        ? "Select a connected repository before sending"
        : localOn && !props.model.trim()
          ? "Choose a local model before sending"
          : undefined;
  const slashState = parseSlashDraft(draft, selectionStart);
  const slashMatches = slashState
    ? matchingSlashCommands(slashState.query, props.agent)
    : [];
  // While the agent is running and the composer is empty, the action button
  // turns into a Stop control (matching Claude Code / Codex). Typing turns it
  // back into a send/queue button.
  const stopMode = props.isRunning && !draft.trim();
  const submit = () => {
    if (!canSend) return;
    if (!props.onSend(draft)) return;
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
  const reposTitle = noRepoSelected
    ? "Select a repository for this run"
    : selectedRepos.length === 0
      ? "Connected repos — none attached yet"
      : `Connected repos: ${selectedRepos.map((repo) => repo.name).join(", ")}`;
  const perm =
    PERMISSIONS.find((item) => item.value === props.permission) ??
    PERMISSIONS[1];
  const permissionLabel = permissionComposerLabel(props.permission);
  const optionsActive =
    sandboxOn ||
    localOn ||
    !!props.model.trim() ||
    (!!props.reasoning && props.reasoning !== "medium");
  const localBaseUrl =
    props.localBaseUrl.trim() ||
    defaultLocalBaseUrl(props.localProvider || "ollama");

  return (
    <footer className="composer">
      <div className="composer-box">
        {slashState && slashMatches.length > 0 && (
          <div className="slash-menu" role="listbox">
            {slashMatches.map((command) => (
              <button
                key={command.name}
                type="button"
                className="slash-item"
                role="option"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  const nextDraft = applySlashCompletion(
                    draft,
                    slashState,
                    command,
                  );
                  setDraft(nextDraft);
                  requestAnimationFrame(() => {
                    const el = textareaRef.current;
                    el?.focus();
                    if (el) {
                      const nextCursor = slashCompletionCursor(
                        slashState,
                        command,
                      );
                      el.setSelectionRange(nextCursor, nextCursor);
                      setSelectionStart(nextCursor);
                      autoGrow(el);
                    }
                  });
                }}
              >
                <span>/{command.name}</span>
                <small>
                  {commandScopeLabel(command) && (
                    <em>{commandScopeLabel(command)}</em>
                  )}
                  {command.description}
                </small>
              </button>
            ))}
          </div>
        )}
        <textarea
          ref={textareaRef}
          value={draft}
          placeholder={
            props.isRunning ? "Ask for follow-up changes" : "Ask anything"
          }
          rows={1}
          onChange={(event) => {
            setDraft(event.target.value);
            setSelectionStart(event.target.selectionStart);
          }}
          onClick={(event) =>
            setSelectionStart(event.currentTarget.selectionStart)
          }
          onKeyUp={(event) =>
            setSelectionStart(event.currentTarget.selectionStart)
          }
          onInput={(event) => autoGrow(event.currentTarget)}
          onKeyDown={(event) => {
            // Enter sends; Shift+Enter (or ⌘/Ctrl+Enter) inserts a newline.
            if (
              event.key === "Enter" &&
              !event.shiftKey &&
              !event.metaKey &&
              !event.ctrlKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault();
              submit();
            }
            if (event.key === "Tab" && slashState && slashMatches[0]) {
              event.preventDefault();
              setDraft(
                applySlashCompletion(draft, slashState, slashMatches[0]),
              );
              requestAnimationFrame(() => {
                const el = textareaRef.current;
                if (!el) return;
                const nextCursor = slashCompletionCursor(
                  slashState,
                  slashMatches[0],
                );
                el.setSelectionRange(nextCursor, nextCursor);
                setSelectionStart(nextCursor);
                autoGrow(el);
              });
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
                  className="composer-icon-btn"
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
                <div className="repo-menu-head">
                  <span className="menu-head">Connected Repos</span>
                  {repos.length > 0 && !props.reposLocked && (
                    <button
                      type="button"
                      className="repo-clear"
                      onClick={() => {
                        setReposOpen(false);
                        props.onClearRepos();
                      }}
                    >
                      Clear all
                    </button>
                  )}
                </div>
                {repos.length === 0 && (
                  <div className="menu-empty">No repositories connected</div>
                )}
                {props.reposLocked && (
                  <div className="menu-empty repo-lock-note">
                    Repositories are fixed after this session creates a managed
                    workspace. Start a new session to use a different set.
                  </div>
                )}
                {repos.map((repo) => {
                  const checked = props.repoIds.includes(repo.id);
                  return (
                    <div key={repo.id} className="repo-row">
                      <label
                        className={
                          checked
                            ? "menu-item check selected"
                            : "menu-item check"
                        }
                        title={
                          props.reposLocked
                            ? "Start a new session to change repositories"
                            : undefined
                        }
                      >
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
                          <small>
                            {repo.kind === "github" ? "GitHub" : "Local"}
                          </small>
                        </span>
                      </label>
                      <button
                        type="button"
                        className="history-del"
                        title={
                          props.reposLocked
                            ? "Start a new session to change repositories"
                            : `Disconnect ${repo.name}`
                        }
                        aria-label={`Disconnect ${repo.name}`}
                        disabled={props.reposLocked}
                        onClick={() => props.onRemoveRepo(repo.id)}
                      >
                        <Icon name="trash" />
                      </button>
                    </div>
                  );
                })}
                <div className="menu-sep" />
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    setReposOpen(false);
                    props.onLocalRepo();
                  }}
                >
                  <Icon name="folder" />
                  <span>Add local folder</span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    setReposOpen(false);
                    props.onGithub();
                  }}
                >
                  <Icon name="github" />
                  <span>Add from GitHub</span>
                </button>
              </div>
            </Popover>

            <Dropdown
              ariaLabel="Agent"
              title="Agent"
              className="chip-btn agent-chip"
              icon={<AgentMark agent={props.agent} />}
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
                    className={
                      option.value === props.permission
                        ? "menu-item selected"
                        : "menu-item"
                    }
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
              setOpen={(open) => {
                setOptionsOpen(open);
                if (!open) setModelOpen(false);
              }}
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
                {modelOpen ? (
                  <ModelBrowser
                    agent={props.agent}
                    snapshot={props.snapshot}
                    localProvider={props.localProvider || null}
                    value={props.model}
                    onBack={() => setModelOpen(false)}
                    onSelect={(nextModel) => {
                      props.setModel(nextModel);
                      props.setReasoning(
                        reasoningAfterModelChange(
                          props.agent,
                          props.snapshot,
                          nextModel,
                          props.reasoning,
                        ),
                      );
                      setModelOpen(false);
                    }}
                  />
                ) : (
                  <>
                    <div className="menu-head">Run options</div>
                    <ModelField
                      agent={props.agent}
                      snapshot={props.snapshot}
                      localProvider={props.localProvider || null}
                      value={props.model}
                      onOpen={() => setModelOpen(true)}
                    />
                    <label className="field">
                      <span>Reasoning effort</span>
                      <select
                        value={props.reasoning}
                        onChange={(event) => props.setReasoning(event.target.value)}
                      >
                        {reasoningOptions(
                          props.agent,
                          props.snapshot,
                          props.model,
                        ).map(
                          (option) => (
                            <option key={option.value} value={option.value}>
                              {option.label}
                            </option>
                          ),
                        )}
                      </select>
                    </label>
                    <button
                      type="button"
                      className={`sandbox-row${sandboxOn ? " on" : ""}${sandboxAllowed ? "" : " disabled"}`}
                      disabled={!sandboxAllowed}
                      aria-pressed={sandboxOn}
                      title={
                        sandboxAllowed
                          ? "Run in an isolated Docker sandbox"
                          : "Sandbox is available for Codex"
                      }
                      onClick={() =>
                        props.setBackend(sandboxOn ? "host" : "docker_sandbox")
                      }
                    >
                      <Icon name={sandboxOn ? "cube" : "cubeOff"} />
                      <span>Docker sandbox</span>
                      <span className="sandbox-state">
                        {sandboxOn ? "On" : "Off"}
                      </span>
                    </button>
                    <button
                      type="button"
                      className={`sandbox-row${localOn ? " on" : ""}${localAllowed ? "" : " disabled"}`}
                      disabled={!localAllowed}
                      aria-pressed={localOn}
                      title={
                        localAllowed
                          ? "Run Codex against a local Ollama or LM Studio model"
                          : "Local model runs use Codex"
                      }
                      onClick={() => {
                        if (localOn) {
                          props.setLocalProvider("");
                          props.setLocalBaseUrl("");
                        } else {
                          const provider =
                            props.snapshot?.defaults.local_provider ?? "ollama";
                          props.setLocalProvider(provider);
                          props.setLocalBaseUrl(
                            props.snapshot?.defaults.local_base_url ??
                              defaultLocalBaseUrl(provider),
                          );
                        }
                      }}
                    >
                      <Icon name="terminal" />
                      <span>Local model</span>
                      <span className="sandbox-state">
                        {localOn ? labelLocalProvider(props.localProvider) : "Off"}
                      </span>
                    </button>
                    {localOn && (
                      <>
                        <label className="field">
                          <span>Local provider</span>
                          <select
                            value={props.localProvider}
                            onChange={(event) => {
                              const provider = event.target
                                .value as LocalModelProvider;
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
                            onChange={(event) =>
                              props.setLocalBaseUrl(event.target.value)
                            }
                          />
                        </label>
                      </>
                    )}
                    {sandboxOn && sandbox && !sandbox.authenticated && (
                      <button
                        type="button"
                        className="menu-item"
                        onClick={() => props.onSandboxLogin(false)}
                      >
                        Sign in to Sandbox
                      </button>
                    )}
                    {sandboxOn && sandbox && !sandbox.codex_authenticated && (
                      <button
                        type="button"
                        className="menu-item"
                        onClick={() => props.onSandboxLogin(true)}
                      >
                        Sign in to Codex Sandbox
                      </button>
                    )}
                  </>
                )}
              </div>
            </Popover>
          </div>

          <button
            className={`send-btn${stopMode ? " stop" : ""}`}
            type="button"
            disabled={!stopMode && !canSend}
            title={
              stopMode
                ? "Stop the agent"
                : sendDisabledReason
                  ? sendDisabledReason
                : props.isRunning
                  ? "Queue message (Enter)"
                  : "Send (Enter)"
            }
            aria-label={
              stopMode ? "Stop" : props.isRunning ? "Queue message" : "Send"
            }
            onClick={stopMode ? props.onStop : submit}
          >
            <Icon
              name={stopMode ? "stop" : props.isRunning ? "queue" : "send"}
            />
          </button>
        </div>
      </div>
    </footer>
  );
}

function AgentMark({ agent }: { agent: AgentKind }) {
  if (agent === "codex") {
    return (
      <svg className="agent-mark codex" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M14.949 6.547a3.94 3.94 0 0 0-.348-3.273 4.11 4.11 0 0 0-4.4-1.934A4.1 4.1 0 0 0 8.423.2 4.15 4.15 0 0 0 6.305.086a4.1 4.1 0 0 0-1.891.948 4.04 4.04 0 0 0-1.158 1.753 4.1 4.1 0 0 0-1.563.679A4 4 0 0 0 .554 4.72a3.99 3.99 0 0 0 .502 4.731 3.94 3.94 0 0 0 .346 3.274 4.11 4.11 0 0 0 4.402 1.933c.382.425.852.764 1.377.995.526.231 1.095.35 1.67.346 1.78.002 3.358-1.132 3.901-2.804a4.1 4.1 0 0 0 1.563-.68 4 4 0 0 0 1.14-1.253 3.99 3.99 0 0 0-.506-4.716m-6.097 8.406a3.05 3.05 0 0 1-1.945-.694l.096-.054 3.23-1.838a.53.53 0 0 0 .265-.455v-4.49l1.366.778q.02.011.025.035v3.722c-.003 1.653-1.361 2.992-3.037 2.996m-6.53-2.75a2.95 2.95 0 0 1-.36-2.01l.095.057L5.29 12.09a.53.53 0 0 0 .527 0l3.949-2.246v1.555a.05.05 0 0 1-.022.041L6.473 13.3c-1.454.826-3.311.335-4.15-1.098m-.85-6.94A3.02 3.02 0 0 1 3.07 3.949v3.785a.51.51 0 0 0 .262.451l3.93 2.237-1.366.779a.05.05 0 0 1-.048 0L2.585 9.342a2.98 2.98 0 0 1-1.113-4.094zm11.216 2.571L8.747 5.576l1.362-.776a.05.05 0 0 1 .048 0l3.265 1.86a3 3 0 0 1 1.173 1.207 2.96 2.96 0 0 1-.27 3.2 3.05 3.05 0 0 1-1.36.997V8.279a.52.52 0 0 0-.276-.445m1.36-2.015-.097-.057-3.226-1.855a.53.53 0 0 0-.53 0L6.249 6.153V4.598a.04.04 0 0 1 .019-.04L9.533 2.7a3.07 3.07 0 0 1 3.257.139c.474.325.843.778 1.066 1.303.223.526.289 1.103.191 1.664zM5.503 8.575 4.139 7.8a.05.05 0 0 1-.026-.037V4.049c0-.57.166-1.127.476-1.607s.752-.864 1.275-1.105a3.08 3.08 0 0 1 3.234.41l-.096.054-3.23 1.838a.53.53 0 0 0-.265.455zm.742-1.577 1.758-1 1.762 1v2l-1.755 1-1.762-1z" />
      </svg>
    );
  }
  return (
    <svg className="agent-mark claude" viewBox="0 0 100 100" aria-hidden="true">
      <path d="m19.6 66.5 19.7-11 .3-1-.3-.5h-1l-3.3-.2-11.2-.3L14 53l-9.5-.5-2.4-.5L0 49l.2-1.5 2-1.3 2.9.2 6.3.5 9.5.6 6.9.4L38 49.1h1.6l.2-.7-.5-.4-.4-.4L29 41l-10.6-7-5.6-4.1-3-2-1.5-2-.6-4.2 2.7-3 3.7.3.9.2 3.7 2.9 8 6.1L37 36l1.5 1.2.6-.4.1-.3-.7-1.1L33 25l-6-10.4-2.7-4.3-.7-2.6c-.3-1-.4-2-.4-3l3-4.2L28 0l4.2.6L33.8 2l2.6 6 4.1 9.3L47 29.9l2 3.8 1 3.4.3 1h.7v-.5l.5-7.2 1-8.7 1-11.2.3-3.2 1.6-3.8 3-2L61 2.6l2 2.9-.3 1.8-1.1 7.7L59 27.1l-1.5 8.2h.9l1-1.1 4.1-5.4 6.9-8.6 3-3.5L77 13l2.3-1.8h4.3l3.1 4.7-1.4 4.9-4.4 5.6-3.7 4.7-5.3 7.1-3.2 5.7.3.4h.7l12-2.6 6.4-1.1 7.6-1.3 3.5 1.6.4 1.6-1.4 3.4-8.2 2-9.6 2-14.3 3.3-.2.1.2.3 6.4.6 2.8.2h6.8l12.6 1 3.3 2 1.9 2.7-.3 2-5.1 2.6-6.8-1.6-16-3.8-5.4-1.3h-.8v.4l4.6 4.5 8.3 7.5L89 80.1l.5 2.4-1.3 2-1.4-.2-9.2-7-3.6-3-8-6.8h-.5v.7l1.8 2.7 9.8 14.7.5 4.5-.7 1.4-2.6 1-2.7-.6-5.8-8-6-9-4.7-8.2-.5.4-2.9 30.2-1.3 1.5-3 1.2-2.5-2-1.4-3 1.4-6.2 1.6-8 1.3-6.4 1.2-7.9.7-2.6v-.2H49L43 72l-9 12.3-7.2 7.6-1.7.7-3-1.5.3-2.8L24 86l10-12.8 6-7.9 4-4.6-.1-.5h-.3L17.2 77.4l-4.7.6-2-2 .2-3 1-1 8-5.5Z" />
    </svg>
  );
}

type SlashCommand = {
  name: string;
  aliases?: string[];
  scopes?: AgentKind[];
  description: string;
  takesInput?: boolean;
};

type AppCommand = SlashCommand & {
  action:
    | "debug"
    | "diff"
    | "effort"
    | "help"
    | "init"
    | "model"
    | "new"
    | "permissions"
    | "plan"
    | "resume"
    | "review"
    | "run"
    | "security-review"
    | "settings"
    | "simplify"
    | "status"
    | "stop"
    | "verify";
};

type AppCommandResolution =
  | {
      kind: "setting";
      setting: "model" | "permission" | "reasoning";
      argument: string;
    }
  | {
      kind: "run";
      permission?: PermissionPolicy;
      requiresWrite?: boolean;
      message: string;
    }
  | {
      kind: "local";
      action: "diff" | "help" | "new" | "resume" | "settings" | "status" | "stop";
    }
  | { kind: "error"; message: string }
  | { kind: "unsupported"; name: string };

// Perpetual owns these commands. They never pass slash text to a headless CLI:
// each one either changes an app run setting or produces a read-only request.
const APP_COMMANDS: AppCommand[] = [
  {
    name: "plan",
    scopes: ["claude_code", "codex"],
    description: "Plan requested work without making changes",
    takesInput: true,
    action: "plan",
  },
  {
    name: "review",
    aliases: ["code-review"],
    scopes: ["claude_code", "codex"],
    description: "Review code or changes without making changes",
    takesInput: true,
    action: "review",
  },
  {
    name: "model",
    scopes: ["claude_code", "codex"],
    description: "Set the model for future runs",
    takesInput: true,
    action: "model",
  },
  {
    name: "permissions",
    aliases: ["permission"],
    scopes: ["claude_code", "codex"],
    description: "Set read-only, write, or full-access mode",
    takesInput: true,
    action: "permissions",
  },
  {
    name: "effort",
    aliases: ["reasoning"],
    scopes: ["claude_code", "codex"],
    description: "Set reasoning effort for future runs",
    takesInput: true,
    action: "effort",
  },
  {
    name: "init",
    scopes: ["claude_code", "codex"],
    description: "Generate provider-native repository guidance",
    takesInput: true,
    action: "init",
  },
  {
    name: "security-review",
    scopes: ["claude_code", "codex"],
    description: "Review pending changes for security risks",
    takesInput: true,
    action: "security-review",
  },
  {
    name: "debug",
    scopes: ["claude_code", "codex"],
    description: "Diagnose and fix a concrete problem",
    takesInput: true,
    action: "debug",
  },
  {
    name: "simplify",
    scopes: ["claude_code", "codex"],
    description: "Simplify changed code and verify the result",
    takesInput: true,
    action: "simplify",
  },
  {
    name: "run",
    scopes: ["claude_code", "codex"],
    description: "Build, launch, and observe the app",
    takesInput: true,
    action: "run",
  },
  {
    name: "verify",
    scopes: ["claude_code", "codex"],
    description: "Verify behavior with builds, tests, and runtime checks",
    takesInput: true,
    action: "verify",
  },
  {
    name: "diff",
    scopes: ["claude_code", "codex"],
    description: "Open Perpetual's current changes view",
    action: "diff",
  },
  {
    name: "status",
    scopes: ["claude_code", "codex"],
    description: "Show Perpetual's active run configuration",
    action: "status",
  },
  {
    name: "new",
    aliases: ["clear", "reset"],
    scopes: ["claude_code", "codex"],
    description: "Start a fresh Perpetual session",
    action: "new",
  },
  {
    name: "resume",
    aliases: ["history"],
    scopes: ["claude_code", "codex"],
    description: "Open Perpetual's session history",
    action: "resume",
  },
  {
    name: "stop",
    scopes: ["claude_code", "codex"],
    description: "Stop the active Perpetual run",
    action: "stop",
  },
  {
    name: "settings",
    aliases: ["config"],
    scopes: ["claude_code", "codex"],
    description: "Open Perpetual settings",
    action: "settings",
  },
  {
    name: "help",
    scopes: ["claude_code", "codex"],
    description: "List commands Perpetual can execute",
    action: "help",
  },
];

function availableSlashCommands(agent: AgentKind): AppCommand[] {
  return APP_COMMANDS.filter(
    (command) => !command.scopes || command.scopes.includes(agent),
  );
}

function commandScopeLabel(command: AppCommand): string {
  if (!command.scopes) return "";
  return command.scopes.map(labelAgent).join(" / ");
}

function parseSlashDraft(
  value: string,
  selectionStart = value.length,
): { query: string; start: number; end: number } | null {
  const cursor = Math.max(0, Math.min(selectionStart, value.length));
  const slashStart = value.match(/^\s*/)?.[0].length ?? 0;
  if (value[slashStart] !== "/" || cursor < slashStart) return null;
  const beforeCursor = value.slice(slashStart, cursor);
  if (!/^\/[A-Za-z-_:]*$/.test(beforeCursor)) return null;
  const afterCursor = value.slice(cursor);
  const suffix = afterCursor.match(/^[A-Za-z-_:]*/)?.[0] ?? "";
  const end = cursor + suffix.length;
  const token = value.slice(slashStart + 1, end);
  if (token.includes("\n")) return null;
  return { query: token.toLowerCase(), start: slashStart, end };
}

function matchingSlashCommands(
  query: string,
  agent: AgentKind,
): AppCommand[] {
  return availableSlashCommands(agent).filter((command) =>
    [command.name, ...(command.aliases ?? [])].some((name) =>
      name.startsWith(query),
    ),
  );
}

function applySlashCompletion(
  value: string,
  slashState: { start: number; end: number },
  command: AppCommand,
): string {
  const replacement = `/${command.name}${command.takesInput ? " " : ""}`;
  return `${value.slice(0, slashState.start)}${replacement}${value.slice(slashState.end)}`;
}

function slashCompletionCursor(
  slashState: { start: number },
  command: AppCommand,
): number {
  return slashState.start + command.name.length + 1 + (command.takesInput ? 1 : 0);
}

function resolveAppCommand(
  value: string,
  agent: AgentKind,
): AppCommandResolution | null {
  const match = /^\s*\/([A-Za-z][A-Za-z-]*)(?:\s+([\s\S]*))?$/.exec(value);
  if (!match) return null;
  const [, rawName, argument = ""] = match;
  const name = rawName.toLowerCase();
  const command = availableSlashCommands(agent).find((candidate) =>
    [candidate.name, ...(candidate.aliases ?? [])].includes(name),
  );
  if (!command) return { kind: "unsupported", name: rawName };

  switch (command.action) {
    case "model":
      return { kind: "setting", setting: "model", argument };
    case "permissions":
      return { kind: "setting", setting: "permission", argument };
    case "effort":
      return { kind: "setting", setting: "reasoning", argument };
    case "plan":
      return {
        kind: "run",
        permission: "read_only",
        message: planPrompt(argument),
      };
    case "review":
      return {
        kind: "run",
        permission: "read_only",
        message: reviewPrompt(argument),
      };
    case "security-review":
      return {
        kind: "run",
        permission: "read_only",
        message: securityReviewPrompt(argument),
      };
    case "init":
      return {
        kind: "run",
        requiresWrite: true,
        message: initPrompt(agent, argument),
      };
    case "debug":
      if (!argument.trim()) {
        return { kind: "error", message: "Usage: /debug <problem>." };
      }
      return {
        kind: "run",
        requiresWrite: true,
        message: debugPrompt(argument),
      };
    case "simplify":
      return {
        kind: "run",
        requiresWrite: true,
        message: simplifyPrompt(argument),
      };
    case "run":
      return {
        kind: "run",
        requiresWrite: true,
        message: runPrompt(argument),
      };
    case "verify":
      return {
        kind: "run",
        requiresWrite: true,
        message: verifyPrompt(argument),
      };
    case "diff":
    case "help":
    case "new":
    case "resume":
    case "settings":
    case "status":
    case "stop":
      return { kind: "local", action: command.action };
  }
}

function permissionFromCommand(value: string): PermissionPolicy | null {
  switch (value.trim().toLowerCase().replace(/[\s_]+/g, "-")) {
    case "read":
    case "read-only":
    case "plan":
      return "read_only";
    case "write":
    case "edit":
    case "workspace-write":
      return "workspace_write";
    case "autonomous":
    case "full-access":
      return "autonomous";
    default:
      return null;
  }
}

function planPrompt(request: string): string {
  const requestedWork =
    request.trim() ||
    "Plan the next appropriate work for the current task and repository state.";
  return `Create an implementation plan for the request below. Inspect the repository as needed, but do not modify files, write files, or perform external actions. If an implementation choice would materially change scope or behavior, ask the user a structured question with concise options before finalizing the plan. Return the scope, affected files, ordered implementation steps, verification plan, and risks or assumptions.\n\nRequest:\n${requestedWork}`;
}

function reviewPrompt(scope: string): string {
  const requestedScope = scope.trim() || "Review the current working tree and recent changes.";
  return `Perform a read-only code review for the scope below. Inspect the relevant repository files and changes, but do not modify files or perform external actions. Report only actionable findings, ordered by severity, with file and line references where possible. If there are no findings, say so plainly.\n\nScope:\n${requestedScope}`;
}

function securityReviewPrompt(scope: string): string {
  const requestedScope =
    scope.trim() || "Review the current working tree and pending changes.";
  return `Perform a read-only security review for the scope below. Inspect the relevant code and diff without modifying files. Focus on exploitable issues such as injection, broken authorization, secret exposure, unsafe deserialization, path traversal, insecure defaults, and data leakage. Report actionable findings ordered by severity with file and line references, impact, and a concise remediation. If there are no findings, say so plainly.\n\nScope:\n${requestedScope}`;
}

function initPrompt(agent: AgentKind, scope: string): string {
  const guidanceFile = agent === "claude_code" ? "CLAUDE.md" : "AGENTS.md";
  const requestedScope = scope.trim() || "the current repository";
  return `Initialize durable agent guidance for ${requestedScope}. Inspect the repository, its existing documentation, package scripts, build files, and tests. Create or update ${guidanceFile} with concise, accurate instructions covering architecture, conventions, common commands, verification steps, and important constraints. Preserve useful existing guidance and do not invent commands you have not verified.`;
}

function debugPrompt(problem: string): string {
  return `Diagnose and fix the concrete problem below. Reproduce it when practical, trace it to its root cause, make the smallest correct change, and run focused verification that demonstrates the fix. Preserve unrelated behavior and report the cause, changes, and evidence.\n\nProblem:\n${problem.trim()}`;
}

function simplifyPrompt(scope: string): string {
  const requestedScope = scope.trim() || "the current working tree changes";
  return `Simplify ${requestedScope} without changing observable behavior. Inspect the surrounding code, remove unnecessary complexity and duplication, reuse established helpers, keep the change narrowly scoped, and run focused tests or builds that verify behavior is preserved.`;
}

function runPrompt(scope: string): string {
  const requestedScope = scope.trim() || "the project's primary application";
  return `Build and launch ${requestedScope}, then exercise the relevant behavior in the running application. Resolve setup or runtime issues that block a meaningful check when they are within the repository, avoid unrelated changes, and report the exact commands, observed behavior, and any remaining limitation.`;
}

function verifyPrompt(scope: string): string {
  const requestedScope = scope.trim() || "the current working tree changes";
  return `Verify ${requestedScope} end to end. Determine the relevant build, test, lint, type-check, and runtime checks from repository guidance and package configuration; run the focused checks that materially prove the behavior; fix in-scope failures; and report concrete results rather than assumptions.`;
}

const PERMISSIONS: {
  value: PermissionPolicy;
  label: string;
  icon: "eye" | "shield" | "bolt";
}[] = [
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
  openSignal: number;
  onLoadDiff(threadId: string): void;
  onApply(threadId: string): void;
  onOpenPath(path: string): void;
}) {
  const [open, setOpen] = useState(false);
  const diffFiles =
    props.diff?.repos.flatMap((repo) =>
      repo.files.map((file) => ({ ...file, repo: repo.repo_name })),
    ) ?? [];
  const hasWorktree = props.repos.some((repo) => !!repo.worktree_path);
  const hasManagedWorktree = props.repos.some(isManagedThreadWorkspace);
  useEffect(() => {
    if (props.openSignal > 0) setOpen(true);
  }, [props.openSignal]);
  if (!hasWorktree || !open) return null;
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
    <div
      className="sheet-backdrop changes-backdrop"
      role="presentation"
      onMouseDown={() => setOpen(false)}
    >
      <section
        className="sheet changes-sheet"
        role="dialog"
        aria-modal="true"
        aria-label="Review changes"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <div className="changes-heading">
            <strong>{summary}</strong>
            {loaded && diffFiles.length > 0 && (
              <span className="changes-stats">
                <span className="diff-add">+{additions}</span>
                <span className="diff-del">-{deletions}</span>
              </span>
            )}
          </div>
          <IconButton title="Close" onClick={() => setOpen(false)}>
            <Icon name="close" />
          </IconButton>
        </header>

        <div className="sheet-body changes-body">
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
            {hasManagedWorktree && (
              <button
                type="button"
                className="primary-btn"
                disabled={loading || (loaded && diffFiles.length === 0)}
                onClick={() => props.onApply(props.threadId)}
              >
                <Icon name="check" />
                <span>Apply to Repo</span>
              </button>
            )}
          </div>
          {props.repos.map((repo) => (
            <div key={repo.repo_id} className="detail-row">
              <Icon name="repo" />
              <span className="detail-name">{repo.repo_name}</span>
              <small>{repo.branch ?? repo.workspace_backend}</small>
              {repo.worktree_path && (
                <button
                  type="button"
                  className="link-btn"
                  onClick={() => props.onOpenPath(repo.worktree_path!)}
                >
                  {isManagedThreadWorkspace(repo) ? "Open Worktree" : "Open Repo"}
                </button>
              )}
            </div>
          ))}

          {loading && <div className="menu-empty">Loading diff...</div>}
          {props.diffState === "error" && (
            <div className="menu-empty">Could not load the diff.</div>
          )}
          {loaded && diffFiles.length === 0 && (
            <div className="menu-empty">No changes to apply.</div>
          )}
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
            <div
              className={
                props.applyResult.applied
                  ? "apply-result ok"
                  : "apply-result blocked"
              }
            >
              <strong>
                {props.applyResult.applied
                  ? "Applied to visible repo"
                  : "Apply blocked"}
              </strong>
              {blockers.length > 0 &&
                blockers.map((blocker) => <span key={blocker}>{blocker}</span>)}
              {props.applyResult.repos.map((repo) => (
                <span key={repo.repo_id}>
                  {repo.repo_name}:{" "}
                  {repo.applied ? "applied" : (repo.blocker ?? "no changes")}
                </span>
              ))}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

function isManagedThreadWorkspace(
  repo: NonNullable<WorkbenchSnapshot["details"]>["repos"][number],
): boolean {
  return Boolean(repo.worktree_path && repo.branch?.startsWith("am/thread-"));
}

function MonitorSheet(props: {
  snapshot: WorkbenchSnapshot;
  selectedThread: AgentThread | null;
  details: ThreadDetails | null;
  onClose(): void;
  onOpenSettings(): void;
  onLaunchCloud(): void;
  onReclaimCloud(): void;
}) {
  const thread = props.selectedThread;
  const activeCloud = props.details?.cloudRuns.find((run) =>
    isActiveCloudRun(run.status),
  );
  const agent = thread?.active_agent ?? thread?.preferred_agent ?? null;
  const activeTurn = props.details?.turns.find((turn) => !turn.ended_at);
  const recentActivities = (props.details?.activities ?? []).slice(-8).reverse();
  const cloudReady =
    !!agent &&
    props.snapshot.cloudAvailability.some(
      (item) => item.agent === agent && item.ready,
    );
  const canLaunchCloud =
    !!thread &&
    !!props.snapshot.cloudPolicy?.enabled &&
    !activeCloud &&
    cloudReady &&
    thread.status !== "draft";

  return (
    <div className="sheet-backdrop" onMouseDown={props.onClose}>
      <section
        className="sheet monitor-sheet"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <strong>Status Monitor</strong>
          <IconButton title="Close" onClick={props.onClose}>
            <Icon name="close" />
          </IconButton>
        </header>
        <div className="sheet-body">
          <div className="monitor-grid">
            <MonitorMetric label="Session" value={thread?.title ?? "New session"} />
            <MonitorMetric label="State" value={thread ? humanize(thread.status) : "Draft"} />
            <MonitorMetric label="Route" value={routeLabel(thread, activeCloud)} />
            <MonitorMetric
              label="Model"
              value={
                thread?.local_provider
                  ? `${prettyModel(thread.model ?? "Local")} via ${labelLocalProvider(thread.local_provider)}`
                  : thread?.model
                    ? prettyModel(thread.model)
                    : "Provider default"
              }
            />
            <MonitorMetric
              label="Backend"
              value={
                thread?.execution_backend === "docker_sandbox"
                  ? `Docker Sandbox${activeTurn?.sandbox_name ? ` · ${activeTurn.sandbox_name}` : ""}`
                  : "Host"
              }
            />
            <MonitorMetric
              label="Limit Reset"
              value={
                thread?.limit_reset_at
                  ? formatResetTime(thread.limit_reset_at)
                  : resetSummary(props.snapshot.agents)
              }
            />
            <MonitorMetric
              label="Queued"
              value={`${props.details?.queued.length ?? 0} follow-up${props.details?.queued.length === 1 ? "" : "s"}`}
            />
            <MonitorMetric
              label="Cloud"
              value={
                activeCloud
                  ? `${labelAgent(activeCloud.agent_kind)} ${humanize(activeCloud.status)}`
                  : props.snapshot.cloudPolicy?.enabled
                    ? "Armed"
                    : "Off"
              }
            />
          </div>

          <div className="settings-group">
            <div className="group-title">Continuity</div>
            <div className="monitor-actions">
              <button
                type="button"
                className="secondary-btn"
                disabled={!canLaunchCloud}
                onClick={props.onLaunchCloud}
              >
                <Icon name="cloud" />
                <span>Launch cloud</span>
              </button>
              <button
                type="button"
                className="secondary-btn"
                disabled={!activeCloud}
                onClick={props.onReclaimCloud}
              >
                <Icon name="download" />
                <span>Reclaim</span>
              </button>
              <button type="button" className="secondary-btn" onClick={props.onOpenSettings}>
                <Icon name="settings" />
                <span>Settings</span>
              </button>
            </div>
          </div>

          <div className="settings-group">
            <div className="group-title">Recent Signals</div>
            <div className="monitor-events">
              {recentActivities.length === 0 && (
                <div className="menu-empty">No handoff or scheduler activity yet</div>
              )}
              {recentActivities.map((activity) => (
                <div key={activity.id} className="monitor-event">
                  <span>{humanize(activity.kind)}</span>
                  <small>{activityDetailText(activity.payload)}</small>
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

function MonitorMetric(props: { label: string; value: string }) {
  return (
    <div className="monitor-metric">
      <small>{props.label}</small>
      <span>{props.value}</span>
    </div>
  );
}

function routeLabel(thread: AgentThread | null, cloud: CloudRun | undefined): string {
  if (cloud) return `${labelAgent(cloud.agent_kind)} Cloud`;
  if (!thread) return "Local";
  if (thread.local_provider) return `Local ${labelLocalProvider(thread.local_provider)}`;
  if (thread.fallback_agent && thread.active_agent === thread.fallback_agent) {
    return `${labelAgent(thread.fallback_agent)} fallback`;
  }
  return `${labelAgent(thread.active_agent ?? thread.preferred_agent)} local`;
}

function resetSummary(agents: AgentStatus[]): string {
  const limited = agents.filter((agent) => agent.availability === "limited");
  if (limited.length === 0) return "No active limits";
  return limited
    .map((agent) =>
      `${labelAgent(agent.kind)} ${agent.reset_at ? formatResetTime(agent.reset_at) : "unknown"}`,
    )
    .join(", ");
}

function activityDetailText(payload: unknown): string {
  const data = asRecord(payload);
  if (!data) return "";
  for (const key of ["reason", "detail", "error", "agent", "url", "queue_id"]) {
    const value = data[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function SettingsSheet(props: {
  snapshot: WorkbenchSnapshot;
  onClose(): void;
  onApply(
    limitPolicy: LimitPolicy,
    sandboxPolicy: SandboxPolicy,
    cloudPolicy: CloudPolicy,
    localModelPolicy: LocalModelPolicy,
  ): void;
  onOpenSettings(): void;
  onOpenExternal(url: string): void;
  onSignInAgent(agent: AgentKind): void;
  onSandboxLogin(codex: boolean): void;
  onGithubSignIn(): void;
  onRefreshReadiness(): void;
}) {
  const [limit, setLimit] = useState<LimitPolicy>(
    () => props.snapshot.limitPolicy ?? defaultLimitPolicy(),
  );
  const [sandbox, setSandbox] = useState<SandboxPolicy>(
    () => props.snapshot.sandboxPolicy ?? defaultSandboxPolicy(),
  );
  const [cloud, setCloud] = useState<CloudPolicy>(
    () => props.snapshot.cloudPolicy ?? defaultCloudPolicy(),
  );
  const [localPolicy, setLocalPolicy] = useState<LocalModelPolicy>(
    () => props.snapshot.localModelPolicy ?? defaultLocalModelPolicy(),
  );
  const cloudClaudeFirst =
    (cloud.provider_priority?.[0] ?? "claude_code") !== "codex";
  return (
    <div className="sheet-backdrop" onMouseDown={props.onClose}>
      <section
        className="sheet"
        onMouseDown={(event) => event.stopPropagation()}
      >
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
                  <small>{agentReadinessLabel(agent, limit)}</small>
                  {agent.installed && !agent.authenticated && (
                    <button
                      type="button"
                      className="readiness-action"
                      onClick={() => props.onSignInAgent(agent.kind)}
                    >
                      Sign in
                    </button>
                  )}
                </div>
              ))}
              {props.snapshot.localModels?.map((provider) => (
                <div key={provider.provider} className="readiness-row">
                  <span>{provider.label}</span>
                  <small>
                    {provider.server_running
                      ? `${provider.models.length} model${provider.models.length === 1 ? "" : "s"}`
                      : "Offline"}
                  </small>
                </div>
              ))}
              {props.snapshot.sandboxRuntime && (
                <div className="readiness-row">
                  <span>Docker Sandbox</span>
                  <small>
	                    {props.snapshot.sandboxRuntime.installed
	                      ? props.snapshot.sandboxRuntime.authenticated
	                        ? props.snapshot.sandboxRuntime.codex_authenticated
	                          ? "Ready"
	                          : "Codex auth required"
	                        : "Not authenticated"
	                      : "Not installed"}
                  </small>
                  {props.snapshot.sandboxRuntime.installed &&
                    !props.snapshot.sandboxRuntime.authenticated && (
                      <button
                        type="button"
                        className="readiness-action"
                        onClick={() => props.onSandboxLogin(false)}
                      >
                        Sign in
                      </button>
                    )}
                  {props.snapshot.sandboxRuntime.installed &&
                    props.snapshot.sandboxRuntime.authenticated &&
                    !props.snapshot.sandboxRuntime.codex_authenticated && (
                      <button
                        type="button"
                        className="readiness-action"
                        onClick={() => props.onSandboxLogin(true)}
                      >
                        Sign in
                      </button>
                    )}
                </div>
              )}
	              <div className="readiness-row">
	                <span>GitHub</span>
	                <small>
	                  {props.snapshot.github?.authenticated
	                    ? "Ready"
	                    : "Use VS Code sign-in"}
	                </small>
	                {!props.snapshot.github?.authenticated && (
	                  <button
	                    type="button"
	                    className="readiness-action"
	                    onClick={props.onGithubSignIn}
	                  >
	                    Sign in
	                  </button>
	                )}
	              </div>
            </div>
            <button
              type="button"
              className="secondary-btn settings-refresh"
              onClick={props.onRefreshReadiness}
            >
              <Icon name="refresh" />
              <span>Refresh readiness</span>
            </button>
          </div>

          <div className="settings-group">
            <div className="group-title">Limit handling</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.auto_switch}
                onChange={(event) =>
                  setLimit({ ...limit, auto_switch: event.target.checked })
                }
              />
              <span>
                Switch to the other agent when the current one is limited
              </span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.resume_with_earliest}
                onChange={(event) =>
                  setLimit({
                    ...limit,
                    resume_with_earliest: event.target.checked,
                  })
                }
              />
              <span>Resume automatically when rate limits reset</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={limit.switch_back}
                disabled={!limit.auto_switch}
                onChange={(event) =>
                  setLimit({ ...limit, switch_back: event.target.checked })
                }
              />
              <span>Return to the original agent after it recovers</span>
            </label>
            <label className="field">
              <span>Retry unknown resets after seconds</span>
              <input
                type="number"
                min={0}
                value={limit.unknown_reset_retry_secs}
                onChange={(event) =>
                  setLimit({
                    ...limit,
                    unknown_reset_retry_secs: Number(event.target.value),
                  })
                }
              />
            </label>
            <div className="field">
              <span>Cloud agent order</span>
              <div className="fallback-order" aria-label="Cloud fallback order">
                {normalizeAgentOrder(limit.agent_priority).map((agent, index) => (
                  <div
                    key={agent}
                    className="fallback-order-row agent-order"
                  >
                    <span>
                      {labelAgent(agent)}
                      <small>Cloud agent</small>
                    </span>
                    <button
                      type="button"
                      title="Move up"
                      disabled={index === 0}
                      onClick={() =>
                        setLimit({
                          ...limit,
                          agent_priority: normalizeAgentOrder(
                            moveItem(
                              normalizeAgentOrder(limit.agent_priority),
                              index,
                              index - 1,
                            ),
                          ),
                        })
                      }
                    >
                      <Icon name="up" />
                    </button>
                    <button
                      type="button"
                      title="Move down"
                      disabled={
                        index ===
                        normalizeAgentOrder(limit.agent_priority).length - 1
                      }
                      onClick={() =>
                        setLimit({
                          ...limit,
                          agent_priority: normalizeAgentOrder(
                            moveItem(
                              normalizeAgentOrder(limit.agent_priority),
                              index,
                              index + 1,
                            ),
                          ),
                        })
                      }
                    >
                      <Icon name="down" />
                    </button>
                  </div>
                ))}
              </div>
            </div>
          </div>

          <div className="settings-group">
            <div className="group-title">Cloud continuity</div>
            <p className="settings-help">
              Set up either cloud here, then turn on continuity to keep work
              running when this machine sleeps or shuts down. Click Apply to
              save your setup.
            </p>
            <div className="cloud-setup-grid">
              <CloudSetupCard
                title="Claude Code on the web"
                ready={props.snapshot.cloudAvailability.find(
                  (item) => item.agent === "claude_code",
                )?.ready ?? false}
                steps={[
                  "Sign in with a Claude.ai subscription in the Claude Code CLI.",
                  "Refresh readiness once sign-in is complete.",
                ]}
                primaryLabel="Sign in to Claude"
                onPrimary={() => props.onSignInAgent("claude_code")}
                secondaryLabel="Open Claude"
                onSecondary={() => props.onOpenExternal("https://claude.ai/code")}
              />
              <CloudSetupCard
                title="Codex Cloud"
                ready={props.snapshot.cloudAvailability.find(
                  (item) => item.agent === "codex",
                )?.ready ?? false}
                steps={[
                  "Sign in to the Codex CLI.",
                  "Create or choose an environment in Codex Cloud, then paste its ID below.",
                ]}
                primaryLabel="Sign in to Codex"
                onPrimary={() => props.onSignInAgent("codex")}
                secondaryLabel="Open Codex Cloud"
                onSecondary={() => props.onOpenExternal("https://chatgpt.com/codex")}
              >
                <label className="field cloud-env-field">
                  <span>Codex environment ID</span>
                  <input
                    value={cloud.codex_env_id ?? ""}
                    placeholder="Paste the environment ID"
                    onChange={(event) =>
                      setCloud({
                        ...cloud,
                        codex_env_id: event.target.value.trim() || null,
                      })
                    }
                  />
                </label>
              </CloudSetupCard>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={cloud.enabled}
                onChange={(event) =>
                  setCloud({ ...cloud, enabled: event.target.checked })
                }
              />
              <span>Auto carryover tasks to the cloud</span>
            </label>
            {cloud.enabled && (
              <>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.continue_on_sleep}
                    onChange={(event) =>
                      setCloud({
                        ...cloud,
                        continue_on_sleep: event.target.checked,
                      })
                    }
                  />
                  <span>Carry over when the machine sleeps</span>
                </label>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.continue_on_shutdown}
                    onChange={(event) =>
                      setCloud({
                        ...cloud,
                        continue_on_shutdown: event.target.checked,
                      })
                    }
                  />
                  <span>Carry over on shutdown</span>
                </label>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={cloud.require_approval}
                    onChange={(event) =>
                      setCloud({
                        ...cloud,
                        require_approval: event.target.checked,
                      })
                    }
                  />
                  <span>Ask before handing work to the cloud</span>
                </label>
                <div className="field">
                  <span>Cloud handoff</span>
                  <div className="segmented">
                    <button
                      type="button"
                      className={!cloud.allow_cross_provider ? "selected" : ""}
                      title="Claude Code tasks continue on Claude Code on the web; Codex tasks on Codex Cloud."
                      onClick={() =>
                        setCloud({ ...cloud, allow_cross_provider: false })
                      }
                    >
                      Same provider
                    </button>
                    <button
                      type="button"
                      className={cloud.allow_cross_provider ? "selected" : ""}
                      title="Hand the task to the other provider's cloud when the current one isn't ready."
                      onClick={() =>
                        setCloud({ ...cloud, allow_cross_provider: true })
                      }
                    >
                      Allow switching
                    </button>
                  </div>
                </div>
                {cloud.allow_cross_provider && (
                  <div className="field">
                    <span>Use first when switching</span>
                    <div className="segmented">
                      <button
                        type="button"
                        className={cloudClaudeFirst ? "selected" : ""}
                        onClick={() =>
                          setCloud({
                            ...cloud,
                            provider_priority: ["claude_code", "codex"],
                          })
                        }
                      >
                        Claude Cloud
                      </button>
                      <button
                        type="button"
                        className={!cloudClaudeFirst ? "selected" : ""}
                        onClick={() =>
                          setCloud({
                            ...cloud,
                            provider_priority: ["codex", "claude_code"],
                          })
                        }
                      >
                        Codex Cloud
                      </button>
                    </div>
                  </div>
                )}
                <div className="field-grid">
                  <label className="field">
                    <span>Max cloud runs</span>
                    <input
                      type="number"
                      min={1}
                      max={8}
                      value={cloud.max_concurrent_cloud_runs}
                      onChange={(event) =>
                        setCloud({
                          ...cloud,
                          max_concurrent_cloud_runs: Number(event.target.value),
                        })
                      }
                    />
                  </label>
                </div>
              </>
            )}
          </div>

          <div className="settings-group">
            <div className="group-title">Local model fallback</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={localPolicy.auto_resume_cloud}
                onChange={(event) =>
                  setLocalPolicy({
                    ...localPolicy,
                    auto_resume_cloud: event.target.checked,
                  })
                }
              />
              <span>Resume cloud agents when the network comes back</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={localPolicy.use_local_fallback}
                onChange={(event) =>
                  setLocalPolicy({
                    ...localPolicy,
                    use_local_fallback: event.target.checked,
                  })
                }
              />
              <span>Use local models while cloud agents are unavailable</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={localPolicy.switch_back_to_cloud}
                disabled={!localPolicy.use_local_fallback}
                onChange={(event) =>
                  setLocalPolicy({
                    ...localPolicy,
                    switch_back_to_cloud: event.target.checked,
                  })
                }
              />
              <span>Switch back from local models when cloud is stable</span>
            </label>
            {localPolicy.use_local_fallback && (
              <>
                <div className="field-grid">
                  <label className="field">
                    <span>Probe every seconds</span>
                    <input
                      type="number"
                      min={5}
                      value={localPolicy.probe_interval_secs}
                      onChange={(event) =>
                        setLocalPolicy({
                          ...localPolicy,
                          probe_interval_secs: Number(event.target.value),
                        })
                      }
                    />
                  </label>
                  <label className="field">
                    <span>Ollama URL</span>
                    <input
                      value={localPolicy.ollama_base_url}
                      onChange={(event) =>
                        setLocalPolicy({
                          ...localPolicy,
                          ollama_base_url: event.target.value,
                        })
                      }
                    />
                  </label>
                  <label className="field">
                    <span>LM Studio URL</span>
                    <input
                      value={localPolicy.lm_studio_base_url}
                      onChange={(event) =>
                        setLocalPolicy({
                          ...localPolicy,
                          lm_studio_base_url: event.target.value,
                        })
                      }
                    />
                  </label>
                </div>
                <div className="field">
                  <span>Fallback models</span>
                  {localPolicy.targets.length > 0 && (
                    <div className="fallback-order" aria-label="Fallback order">
                      {localPolicy.targets.map((target, index) => (
                        <div
                          key={`${target.provider}:${target.model}:${index}`}
                          className="fallback-order-row"
                        >
                          <span>
                            {prettyModel(target.model)}
                            <small>{labelLocalProvider(target.provider)}</small>
                          </span>
                          <button
                            type="button"
                            title="Move up"
                            disabled={index === 0}
                            onClick={() =>
                              setLocalPolicy({
                                ...localPolicy,
                                targets: moveItem(localPolicy.targets, index, index - 1),
                              })
                            }
                          >
                            <Icon name="up" />
                          </button>
                          <button
                            type="button"
                            title="Move down"
                            disabled={index === localPolicy.targets.length - 1}
                            onClick={() =>
                              setLocalPolicy({
                                ...localPolicy,
                                targets: moveItem(localPolicy.targets, index, index + 1),
                              })
                            }
                          >
                            <Icon name="down" />
                          </button>
                          <button
                            type="button"
                            title="Remove"
                            onClick={() =>
                              setLocalPolicy({
                                ...localPolicy,
                                targets: localPolicy.targets.filter(
                                  (_, itemIndex) => itemIndex !== index,
                                ),
                              })
                            }
                          >
                            <Icon name="trash" />
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                  <div className="model-targets">
                    {props.snapshot.localModels?.flatMap((provider) =>
                      provider.models.map((modelInfo) => {
                        const active = localPolicy.targets.some(
                          (target) =>
                            target.provider === provider.provider &&
                            target.model === modelInfo.id,
                        );
                        return (
                          <button
                            key={`${provider.provider}:${modelInfo.id}`}
                            type="button"
                            className={active ? "selected" : ""}
                            onClick={() => {
                              const exists = localPolicy.targets.some(
                                (target) =>
                                  target.provider === provider.provider &&
                                  target.model === modelInfo.id,
                              );
                              const targets = exists
                                ? localPolicy.targets.filter(
                                    (target) =>
                                      !(
                                        target.provider === provider.provider &&
                                        target.model === modelInfo.id
                                      ),
                                  )
                                : [
                                    ...localPolicy.targets,
                                    {
                                      provider: provider.provider,
                                      model: modelInfo.id,
                                      base_url: provider.base_url,
                                    },
                                  ];
                              setLocalPolicy({ ...localPolicy, targets });
                            }}
                          >
                            {modelInfo.name || modelInfo.id}
                            <small>{provider.label}</small>
                          </button>
                        );
                      }),
                    )}
                    {!(props.snapshot.localModels ?? []).some(
                      (provider) => provider.models.length > 0,
                    ) && (
                      <div className="menu-empty">No local models detected</div>
                    )}
                  </div>
                </div>
              </>
            )}
          </div>

          <div className="settings-group">
            <div className="group-title">Docker Sandbox</div>
            <label className="field">
              <span>Default runtime</span>
              <select
                value={sandbox.default_backend}
                onChange={(event) =>
                  setSandbox({
                    ...sandbox,
                    default_backend: event.target.value as ExecutionBackend,
                  })
                }
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
                  onChange={(event) =>
                    setSandbox({
                      ...sandbox,
                      max_concurrent_sandboxes: Number(event.target.value),
                    })
                  }
                />
              </label>
              <label className="field">
                <span>CPUs</span>
                <input
                  type="number"
                  min={1}
                  max={16}
                  value={sandbox.cpus}
                  onChange={(event) =>
                    setSandbox({ ...sandbox, cpus: Number(event.target.value) })
                  }
                />
              </label>
              <label className="field">
                <span>Memory</span>
                <input
                  value={sandbox.memory}
                  onChange={(event) =>
                    setSandbox({ ...sandbox, memory: event.target.value })
                  }
                />
              </label>
              <label className="field">
                <span>Network</span>
                <select
                  value={sandbox.network_preset}
                  onChange={(event) =>
                    setSandbox({
                      ...sandbox,
                      network_preset: event.target.value,
                    })
                  }
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
          <button
            type="button"
            className="primary"
            onClick={() => props.onApply(limit, sandbox, cloud, localPolicy)}
          >
            Apply
          </button>
        </footer>
      </section>
    </div>
  );
}

function CloudSetupCard(props: {
  title: string;
  ready: boolean;
  steps: string[];
  primaryLabel: string;
  onPrimary(): void;
  secondaryLabel: string;
  onSecondary(): void;
  children?: ReactNode;
}) {
  return (
    <div className="cloud-setup-card">
      <div className="cloud-setup-heading">
        <strong>{props.title}</strong>
        <span className={props.ready ? "cloud-status ready" : "cloud-status"}>
          {props.ready ? "Ready" : "Set up"}
        </span>
      </div>
      <ol className="cloud-setup-steps">
        {props.steps.map((step) => (
          <li key={step}>{step}</li>
        ))}
      </ol>
      {props.children}
      <div className="cloud-setup-actions">
        <button type="button" className="readiness-action" onClick={props.onPrimary}>
          {props.primaryLabel}
        </button>
        <button type="button" className="readiness-action" onClick={props.onSecondary}>
          {props.secondaryLabel}
        </button>
      </div>
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
  const filtered = props.repos.filter((repo) =>
    repo.full_name.toLowerCase().includes(query.toLowerCase()),
  );
  return (
    <div className="sheet-backdrop" onMouseDown={props.onClose}>
      <section
        className="sheet repo-sheet"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <strong>Add from GitHub</strong>
          <IconButton title="Close" onClick={props.onClose}>
            <Icon name="close" />
          </IconButton>
        </header>
        <div className="sheet-search">
          <Icon name="search" />
          <input
            autoFocus
            placeholder="Filter repositories"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
        <div className="github-list">
          {props.loading && (
            <div className="menu-empty">Loading repositories…</div>
          )}
          {!props.loading && filtered.length === 0 && (
            <div className="menu-empty">No matching repositories</div>
          )}
          {!props.loading &&
            filtered.map((repo) => (
              <button
                key={repo.id}
                type="button"
                className="github-row"
                onClick={() => props.onConnect(repo)}
              >
                <Icon name={repo.private ? "lock" : "github"} />
                <span className="history-text">
                  <span>{repo.full_name}</span>
                  <small>
                    {repo.private ? "Private" : "Public"} ·{" "}
                    {repo.default_branch}
                  </small>
                </span>
              </button>
            ))}
        </div>
      </section>
    </div>
  );
}

function EmptyState({
  compact = false,
  exiting = false,
}: {
  compact?: boolean;
  exiting?: boolean;
}) {
  return (
    <div
      className={
        compact
          ? "empty compact"
          : `empty welcome${exiting ? " is-leaving" : ""}`
      }
      aria-hidden={exiting || undefined}
    >
      <span className="empty-mark">
        <BrandMark size={240} />
      </span>
    </div>
  );
}

function autoGrow(el: HTMLTextAreaElement) {
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 180)}px`;
}

function runDefaults(snapshot: WorkbenchSnapshot, agent: AgentKind) {
  return (
    snapshot.runDefaults.find((item) => item.kind === agent) ?? {
      model: null,
      reasoning: null,
    }
  );
}

type PickerModelOption = {
  value: string;
  label: string;
  source: string;
};

export function modelOptions(
  agent: AgentKind,
  snapshot: WorkbenchSnapshot | null,
  localProvider: LocalModelProvider | null,
  current: string,
): PickerModelOption[] {
  const out: PickerModelOption[] = [];
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  if (agent === "codex" && localProvider) {
    const status = snapshot?.localModels?.find(
      (item) => item.provider === localProvider,
    );
    for (const model of status?.models ?? []) {
      pushPickerOption(out, {
        value: model.id,
        label: model.name || prettyModel(model.id),
        source: `${status?.label ?? labelLocalProvider(localProvider)}${model.loaded ? " loaded" : ""}`,
      });
    }
  } else {
    for (const option of catalog?.models ?? []) {
      if (!option.available) continue;
      pushPickerOption(out, catalogOption(option));
    }
    const detectedModels = (catalog?.models ?? []).some(
      (option) => option.source !== "settings" && option.available,
    );
    if (!detectedModels) {
      for (const option of fallbackModelOptions(agent)) {
        pushPickerOption(out, option);
      }
    }
  }

  const defaults = snapshot
    ? runDefaults(snapshot, agent)
    : { model: null, reasoning: null };
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
      source: out.some((option) => modelIdsEqual(option.value, current))
        ? "Detected"
        : "Custom",
    });
  }
  return out;
}

function fallbackModelOptions(agent: AgentKind): PickerModelOption[] {
  const models =
    agent === "claude_code"
      ? ["opus", "sonnet", "haiku"]
      : ["gpt-5-codex", "gpt-5", "gpt-4.1", "o3", "o4-mini"];
  return models.map((value) => ({
    value,
    label: prettyModel(value),
    source: "Built-in fallback",
  }));
}

function catalogOption(option: AgentModelOption): PickerModelOption {
  return {
    value: option.id,
    label: option.label || prettyModel(option.id),
    source: option.default
      ? `${sourceLabel(option.source)} default`
      : sourceLabel(option.source),
  };
}

function pushPickerOption(
  out: PickerModelOption[],
  option: PickerModelOption,
): void {
  if (!option.value.trim()) return;
  const existing = out.find((item) => modelIdsEqual(item.value, option.value));
  if (existing) {
    if (existing.source === "Custom") existing.source = option.source;
    return;
  }
  out.push(option);
}

function moveItem<T>(items: T[], from: number, to: number): T[] {
  if (to < 0 || to >= items.length || from === to) return items;
  const next = [...items];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

function normalizeAgentOrder(value: readonly AgentKind[] | null | undefined): AgentKind[] {
  const out: AgentKind[] = [];
  for (const agent of value ?? []) {
    if ((agent === "claude_code" || agent === "codex") && !out.includes(agent)) {
      out.push(agent);
    }
  }
  for (const agent of ["claude_code", "codex"] as const) {
    if (!out.includes(agent)) out.push(agent);
  }
  return out;
}

export function reasoningOptions(
  agent: AgentKind,
  snapshot: WorkbenchSnapshot | null,
  model: string,
): { value: string; label: string }[] {
  const values = [""];
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  const selectedModel = catalog?.models.find(
    (option) =>
      modelIdsEqual(option.id, model) ||
      option.aliases?.some((alias) => modelIdsEqual(alias, model)),
  );
  const detected = selectedModel?.reasoning?.length
    ? selectedModel.reasoning
    : (catalog?.reasoning ?? []);
  for (const value of detected) pushReasoning(values, value);
  const defaults = snapshot
    ? runDefaults(snapshot, agent)
    : { model: null, reasoning: null };
  if (!selectedModel?.reasoning?.length && defaults.reasoning) {
    pushReasoning(values, defaults.reasoning);
  }
  // Fall back only when the installed CLI exposed no effort metadata. Once it
  // does, its values are authoritative and automatically include future names.
  if (detected.length === 0) {
    for (const fallback of agent === "claude_code"
      ? ["low", "medium", "high", "xhigh", "max"]
      : ["low", "medium", "high"]) {
      pushReasoning(values, fallback);
    }
  }
  return values.map((value) => ({
    value,
    label: value ? humanize(value) : "Default",
  }));
}

export function reasoningAfterModelChange(
  agent: AgentKind,
  snapshot: WorkbenchSnapshot | null,
  model: string,
  current: string,
): string {
  const options = reasoningOptions(agent, snapshot, model);
  const retained = options.find(
    (option) => option.value.toLowerCase() === current.trim().toLowerCase(),
  );
  if (retained) return retained.value;
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  const selectedModel = catalog?.models.find(
    (option) =>
      modelIdsEqual(option.id, model) ||
      option.aliases?.some((alias) => modelIdsEqual(alias, model)),
  );
  return selectedModel?.default_reasoning?.trim() ?? "";
}

function pushReasoning(values: string[], value: string): void {
  const trimmed = value.trim();
  if (!trimmed) return;
  if (!values.some((item) => item.toLowerCase() === trimmed.toLowerCase()))
    values.push(trimmed);
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

export function sameStringSet(
  a: readonly string[],
  b: readonly string[],
): boolean {
  return a.length === b.length && a.every((value) => b.includes(value));
}

// Strips provider decorations from a model id — e.g. the "[1m]" 1M-context
// suffix Claude Code appends — so display and dedupe work on the base id.
// The raw id (suffix included) is still what gets passed to the CLI.
function baseModelId(value: string): string {
  return value
    .trim()
    .replace(/\[[^\]]*\]$/, "")
    .trim();
}

function sanitizeModelForAgent(
  _agent: AgentKind,
  model: string,
  localProvider: LocalModelProvider | null,
): string | null {
  const trimmed = model.trim();
  if (!trimmed) return null;
  return trimmed;
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
  return v
    .replace(/\bgpt\b/gi, "GPT")
    .replace(
      /(^|[\s\-_])([a-z])/g,
      (_match, sep, ch) => sep + ch.toUpperCase(),
    );
}

function formatModelSwitch(
  from: string | null | undefined,
  to: string | null | undefined,
): string {
  const fromLabel = from ? prettyModel(from) : "default model";
  const toLabel = to ? prettyModel(to) : "default model";
  return `${fromLabel} -> ${toLabel}`;
}

function labelLocalProvider(provider: LocalModelProvider | ""): string {
  return provider === "lm_studio"
    ? "LM Studio"
    : provider === "ollama"
      ? "Ollama"
      : "Off";
}

function defaultLocalBaseUrl(provider: LocalModelProvider): string {
  return provider === "lm_studio"
    ? "http://127.0.0.1:1234"
    : "http://127.0.0.1:11434";
}

function agentReadinessLabel(
  agent: AgentStatus,
  policy: LimitPolicy | null,
): string {
  if (!agent.installed) return "Not installed";
  if (!agent.authenticated) return "Not authenticated";
  if (agent.availability === "limited") {
    return agent.reset_at
      ? `Limited until ${formatResetTime(agent.reset_at)}`
      : `Limited - ${retryLabel(policy)}`;
  }
  return humanize(agent.availability);
}

function retryLabel(policy: LimitPolicy | null): string {
  const retry = policy?.unknown_reset_retry_secs ?? 600;
  if (retry <= 0) return "reset time unknown";
  return `reset unknown, rechecking every ${formatDuration(retry * 1000)}`;
}

function formatResetTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "reset time unavailable";
  const clock = date.toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
  return `${clock} (${relativeReset(date)})`;
}

function relativeReset(date: Date): string {
  const diff = date.getTime() - Date.now();
  if (diff <= 0) return "ready now";
  return `in ${formatDuration(diff)}`;
}

function formatDuration(ms: number): string {
  const totalMinutes = Math.max(1, Math.round(ms / 60_000));
  if (totalMinutes < 60) return `${totalMinutes}m`;
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
}

function sanitizeBackend(
  agent: AgentKind,
  backend: ExecutionBackend,
): ExecutionBackend {
  return backend === "docker_sandbox" && agent !== "codex" ? "host" : backend;
}

function labelAgent(agent: AgentKind | null | undefined): string {
  switch (agent) {
    case "claude_code":
      return "Claude";
    case "codex":
      return "Codex";
    default:
      return "Agent";
  }
}

function isActivityEvent(event: AgentThreadEvent): boolean {
  return (
    event.role === "tool" || event.role === "app" || event.role === "system"
  );
}

function activityIcon(event: AgentThreadEvent) {
  if (event.kind === "file_changed") return "repo" as const;
  if (event.kind === "token_usage") return "clock" as const;
  if (
    event.kind === "usage_limit" ||
    event.kind === "network_unavailable" ||
    event.kind === "error"
  )
    return "alert" as const;
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
  if (event.kind === "session_ended")
    return `Session ${String(event.text ?? "ended").toLowerCase()}`;
  if (event.kind === "usage_limit") return event.text ?? "Usage limit reached";
  if (event.kind === "network_unavailable")
    return event.text ?? "Network unavailable";
  if (event.kind === "awaiting_approval")
    return event.text ?? "Awaiting approval";
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
        ? JSON.stringify(publicToolData(data.input), null, 2)
        : JSON.stringify(publicToolData(data), null, 2);
  if (!detail || detail === "{}") return null;
  return truncateDetail(detail, 1800);
}

function publicToolData(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(publicToolData);
  const record = asRecord(value);
  if (!record) return value;
  const hidden = new Set([
    "prompt",
    "system_prompt",
    "systemPrompt",
    "developer_message",
    "developerMessage",
    "instructions",
  ]);
  return Object.fromEntries(
    Object.entries(record)
      .filter(([key]) => !hidden.has(key))
      .map(([key, child]) => [key, publicToolData(child)]),
  );
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function toolPath(value: unknown): string | null {
  const input = asRecord(value);
  const candidate =
    input?.file_path ?? input?.path ?? input?.filepath ?? input?.filename;
  return typeof candidate === "string" && candidate.trim()
    ? candidate.trim()
    : null;
}

function toolCommand(value: unknown): string | null {
  const input = asRecord(value);
  const candidate = input?.command;
  if (typeof candidate === "string") return truncateDetail(candidate, 120);
  if (Array.isArray(candidate)) return truncateDetail(candidate.join(" "), 120);
  return null;
}

function truncateDetail(value: string, max: number): string {
  return value.length <= max
    ? value
    : `${value.slice(0, max)}\n\n[details truncated]`;
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
    provider_priority: ["claude_code", "codex"],
    checkpoint_interval_secs: 120,
    monitor_poll_secs: 30,
    stall_timeout_secs: 900,
    max_concurrent_cloud_runs: 2,
    codex_env_id: null,
    require_approval: false,
  };
}

function isActiveCloudRun(status: string): boolean {
  return status === "provisioning" || status === "running" || status === "stalled";
}

function defaultLocalModelPolicy(): LocalModelPolicy {
  return {
    auto_resume_cloud: true,
    use_local_fallback: true,
    switch_back_to_cloud: true,
    probe_interval_secs: 30,
    offline_grace_secs: 15,
    stable_successes: 2,
    ollama_base_url: "http://127.0.0.1:11434",
    lm_studio_base_url: "http://127.0.0.1:1234",
    lm_studio_api_token_configured: false,
    lm_studio_api_token: null,
    targets: [],
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
