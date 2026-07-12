import type {
  ActivityEvent,
  AgentKind,
  AgentThread,
  AgentThreadEvent,
  CloudRun,
  QueuedTurn,
} from "./types";

/** Icon names the transcript may ask the view for, so the glyph carries the
 * meaning (a clock waits, an alert failed) rather than merely echoing the tone. */
export type TransitionIcon =
  | "clock"
  | "refresh"
  | "alert"
  | "queue"
  | "cloud"
  | "terminal";

export type TranscriptItem =
  | { type: "event"; event: AgentThreadEvent }
  | {
      type: "transition";
      id: string;
      tone: "info" | "warning" | "danger";
      text: string;
      detail?: string | null;
      icon?: TransitionIcon;
    }
  | { type: "queued"; id: string; message: string }
  | { type: "pending"; id: string; text: string; firstTurn?: boolean };

export type PendingTranscriptMessage = {
  id: string;
  text: string;
  firstTurn?: boolean;
};

export function reconcilePendingMessages(input: {
  pending: PendingTranscriptMessage[];
  selectedStatus: AgentThread["status"] | null | undefined;
  events: AgentThreadEvent[];
  queued: QueuedTurn[];
}): PendingTranscriptMessage[] {
  if (!input.selectedStatus || input.selectedStatus === "draft") {
    return input.pending;
  }
  return input.pending.filter((item) => {
    // Keep the optimistic first bubble mounted while the welcome screen exits.
    // The persisted duplicate is hidden by the view until assistant output
    // starts, which gives the layout one stable element to animate.
    if (
      item.firstTurn &&
      input.selectedStatus === "running" &&
      !input.events.some((event) => event.role === "assistant")
    ) {
      return true;
    }
    if (
      input.events.some((event) => eventClientMessageId(event) === item.id) ||
      input.queued.some((turn) => turn.client_message_id === item.id)
    ) {
      return false;
    }
    // Legacy snapshots did not include client ids; only use text matching when
    // no stable id exists on either side.
    const legacyEventMatch = input.events.some(
      (event) =>
        !eventClientMessageId(event) &&
        event.role === "user" &&
        (event.text ?? "").trim() === item.text,
    );
    if (legacyEventMatch) return false;
    return !input.queued.some(
      (turn) => !turn.client_message_id && turn.message.trim() === item.text,
    );
  });
}

export function buildTranscriptItems(input: {
  thread: AgentThread | null;
  events: AgentThreadEvent[];
  activities: ActivityEvent[];
  queued: QueuedTurn[];
  cloudRuns: CloudRun[];
  pending?: PendingTranscriptMessage[];
}): TranscriptItem[] {
  const items: TranscriptItem[] = [];
  const transitionIds = new Set<string>();
  const noAssistantUserTexts = new Set<string>();
  let suppressedUsageLimit = false;
  let limitTransitionAdded = false;

  for (const event of input.events) {
    if (isRoutineEvent(event)) {
      if (event.kind === "usage_limit") suppressedUsageLimit = true;
      continue;
    }
    if (event.role === "user") {
      const text = publicUserMessage(event.text ?? "").trim();
      if (text && noAssistantUserTexts.has(text)) continue;
      if (text) noAssistantUserTexts.add(text);
      items.push({
        type: "event",
        event: text === event.text ? event : { ...event, text },
      });
      continue;
    } else if (event.role === "assistant" || event.role === "tool") {
      noAssistantUserTexts.clear();
    }
    items.push({ type: "event", event });
  }

  const superseded = supersededActivities(input.activities);
  for (const activity of input.activities) {
    if (superseded.has(activity.id)) {
      limitTransitionAdded = true;
      continue;
    }
    const transition = transitionFromActivity(activity);
    if (!transition) continue;
    if (isLimitActivity(activity.kind)) limitTransitionAdded = true;
    transitionIds.add(transition.id);
    items.push(transition);
  }

  if (suppressedUsageLimit && !limitTransitionAdded && input.thread) {
    const fallback = input.thread.fallback_agent;
    const original = input.thread.original_agent ?? input.thread.preferred_agent;
    const text = fallback
      ? `${labelAgent(original)} rate-limited; switching to ${labelAgent(fallback)}`
      : `${labelAgent(original)} rate-limited; waiting for the reset`;
    items.push({
      type: "transition",
      id: `usage-limit-${input.thread.id}`,
      tone: fallback ? "warning" : "danger",
      text,
      detail: input.thread.limit_reset_at
        ? `Resets at ${formatReset(input.thread.limit_reset_at)}`
        : null,
      icon: fallback ? "refresh" : "clock",
    });
  }

  for (const run of input.cloudRuns) {
    if (!isActiveCloudRun(run.status)) continue;
    const id = `cloud-active-${run.id}`;
    if (transitionIds.has(id)) continue;
    items.push({
      type: "transition",
      id,
      tone: run.status === "stalled" ? "warning" : "info",
      text: `Continuing in ${labelAgent(run.agent_kind)} Cloud`,
      detail: run.url ?? run.branch,
      icon: "cloud",
    });
  }

  for (const queued of input.queued) {
    if (queued.echo_user_message === false) continue;
    items.push({ type: "queued", id: queued.id, message: queued.message });
  }

  for (const message of input.pending ?? []) {
    items.push({
      type: "pending",
      id: message.id,
      text: message.text,
      firstTurn: message.firstTurn,
    });
  }

  const anchors = sendTimeAnchors(input.activities, input.events);
  return items.sort(
    (a, b) => itemTs(a, input, anchors) - itemTs(b, input, anchors),
  );
}

/**
 * Some transitions announce something they immediately resolve, and the outcome
 * already carries the news: "Codex rate-limited" next to "Codex rate-limited;
 * switching to Claude" says it twice, as does "Codex is available again;
 * switching back" next to "Back on Codex". Keep only the outcome. An opener that
 * never resolves still speaks for itself, which is what makes it worth showing.
 */
function supersededActivities(activities: ActivityEvent[]): Set<string> {
  const ordered = [...activities].sort(
    (a, b) => (Date.parse(a.ts) || 0) - (Date.parse(b.ts) || 0),
  );
  const superseded = new Set<string>();
  ordered.forEach((activity, index) => {
    const agent = openerAgent(activity);
    if (!agent) return;
    for (const later of ordered.slice(index + 1)) {
      // A repeat of the same opener starts a new episode: stop looking, so an
      // earlier unresolved opener is never cancelled by a later one's outcome.
      if (later.kind === activity.kind && openerAgent(later) === agent) break;
      if (!resolves(activity.kind, later, agent)) continue;
      superseded.add(activity.id);
      break;
    }
  });
  return superseded;
}

/** The agent an opening transition is about, or null if it does not open one. */
function openerAgent(activity: ActivityEvent): AgentKind | null {
  const payload = asRecord(activity.payload);
  if (activity.kind === "thread.agent_limited") return parseAgent(payload.agent);
  if (activity.kind === "thread.switchback_started") return parseAgent(payload.to);
  return null;
}

function resolves(
  opener: string,
  activity: ActivityEvent,
  agent: AgentKind | null,
): boolean {
  const payload = asRecord(activity.payload);
  if (opener === "thread.agent_limited") {
    switch (activity.kind) {
      case "thread.fallback_started":
        return parseAgent(payload.from) === agent;
      case "thread.fallback_waiting":
      case "thread.fallback_disabled":
        return parseAgent(payload.agent) === agent;
      default:
        return false;
    }
  }
  if (opener === "thread.switchback_started") {
    return (
      activity.kind === "thread.switchback_completed" &&
      parseAgent(payload.agent) === agent
    );
  }
  return false;
}

/**
 * The daemon checks limits while starting a turn, which happens before the
 * user's message is persisted, so those activities carry an earlier timestamp
 * than the message that triggered them. Anchoring them to that message keeps the
 * transcript in the order the user experienced it: they ask, the workbench says
 * the agent is limited and what it is switching to, then the answer arrives.
 * Only send-time checks carry one of these reasons; a limit hit mid-run is
 * already in the right place.
 */
const SEND_TIME_LIMIT_REASONS = new Set([
  "preflight",
  "known_limited",
  "no_ready_fallback",
  "auto_switch_disabled",
]);

function sendTimeAnchors(
  activities: ActivityEvent[],
  events: AgentThreadEvent[],
): Map<string, number> {
  const userTimes = events
    .filter((event) => event.role === "user" && !isRoutineEvent(event))
    .map((event) => Date.parse(event.ts) || 0)
    .sort((a, b) => a - b);
  const anchors = new Map<string, number>();
  for (const activity of activities) {
    if (!isLimitActivity(activity.kind)) continue;
    const reason = asRecord(activity.payload).reason;
    if (typeof reason !== "string" || !SEND_TIME_LIMIT_REASONS.has(reason)) {
      continue;
    }
    const ts = Date.parse(activity.ts) || 0;
    const trigger = userTimes.find((time) => time >= ts);
    // Until the message is persisted it is still the optimistic bubble at the
    // end of the transcript, so trail everything rather than briefly sorting
    // above it and then jumping once the real event lands.
    // Half a millisecond lands the notice after the message but before any reply.
    anchors.set(
      `activity-${activity.id}`,
      trigger === undefined ? TRAILING_RANK : trigger + 0.5,
    );
  }
  return anchors;
}

function isRoutineEvent(event: AgentThreadEvent): boolean {
  // Provider/system envelopes are operational metadata, never conversation.
  // Keeping this boundary here prevents a newly introduced system-prompt event
  // from accidentally becoming visible just because the renderer understands it.
  if (event.role === "system") return true;
  if (event.kind === "session_started") return true;
  if (event.kind === "token_usage") return true;
  if (event.kind === "usage_limit") return true;
  if (event.kind === "session_ended") {
    const status = String((event.data as { status?: unknown } | null)?.status ?? event.text ?? "")
      .toLowerCase();
    return status === "completed" || status === "interrupted" || status === "failed";
  }
  return false;
}

/**
 * App-owned slash commands are expanded into provider instructions before they
 * leave the webview. Persisted transcripts must show what the user typed, not
 * those internal implementation prompts.
 */
export function publicUserMessage(value: string): string {
  const text = value.trim();
  const commands: Array<[RegExp, string]> = [
    [/^Create an implementation plan[\s\S]*?\n\nRequest:\n([\s\S]+)$/i, "plan"],
    [/^Perform a read-only code review[\s\S]*?\n\nScope:\n([\s\S]+)$/i, "review"],
    [/^Perform a read-only security review[\s\S]*?\n\nScope:\n([\s\S]+)$/i, "security-review"],
    [/^Diagnose and fix the concrete problem[\s\S]*?\n\nProblem:\n([\s\S]+)$/i, "debug"],
  ];
  for (const [pattern, command] of commands) {
    const match = pattern.exec(text);
    if (match) return `/${command} ${match[1].trim()}`;
  }
  if (/^Initialize durable agent guidance for /i.test(text)) return "/init";
  if (/^Simplify [\s\S]+ without changing observable behavior\./i.test(text)) return "/simplify";
  if (/^Build and launch [\s\S]+, then exercise the relevant behavior/i.test(text)) return "/run";
  if (/^Verify [\s\S]+ end to end\./i.test(text)) return "/verify";
  return value;
}

type TransitionItem = Extract<TranscriptItem, { type: "transition" }>;

function transitionFromActivity(activity: ActivityEvent): TransitionItem | null {
  const payload = asRecord(activity.payload);
  const agent = parseAgent(payload.agent);
  const from = parseAgent(payload.from);
  const to = parseAgent(payload.to);
  const reset = typeof payload.reset_at === "string" ? payload.reset_at : null;
  const detail = reset ? `Resets at ${formatReset(reset)}` : detailText(payload);
  switch (activity.kind) {
    case "thread.agent_limited":
      return transition(activity, "warning", `${labelAgent(agent)} rate-limited`, detail, "clock");
    case "thread.fallback_started":
      return transition(
        activity,
        "warning",
        `${labelAgent(from)} rate-limited; switching to ${labelAgent(to)}`,
        detail,
        "refresh",
      );
    case "thread.fallback_waiting":
      return transition(
        activity,
        "danger",
        `${labelAgent(agent)} rate-limited; waiting for the reset`,
        detail,
        "clock",
      );
    case "thread.fallback_disabled":
      return transition(
        activity,
        "danger",
        `${labelAgent(agent)} rate-limited; auto-switch is off`,
        detail,
        "clock",
      );
    case "thread.switchback_started":
      return transition(
        activity,
        "info",
        `${labelAgent(to)} is available again; switching back`,
        null,
        "refresh",
      );
    case "thread.switchback_completed":
      return transition(activity, "info", `Back on ${labelAgent(agent)}`, null, "refresh");
    case "thread.capacity_queued":
      return transition(
        activity,
        "info",
        `Waiting for a free ${labelAgent(agent)} slot`,
        null,
        "queue",
      );
    case "thread.context_handoff":
      return transition(
        activity,
        "info",
        `Context limit reached; continuing in a fresh ${labelAgent(agent)} session`,
        null,
        "refresh",
      );
    case "thread.network_unavailable":
      return transition(
        activity,
        "warning",
        `${labelAgent(agent)} lost its network connection`,
        detailText(payload),
        "alert",
      );
    case "thread.network_waiting":
      return transition(activity, "warning", "Waiting for the network to come back", null, "clock");
    case "thread.local_fallback_started":
      return transition(
        activity,
        "warning",
        `Cloud unavailable; switching to local ${labelAgent(to)}`,
        localProviderLabel(payload) ?? detail,
        "terminal",
      );
    case "thread.local_fallback_failed":
      return transition(activity, "danger", "Local fallback failed", detail, "alert");
    case "thread.local_switchback_started":
      return transition(
        activity,
        "info",
        `Network is back; resuming ${labelAgent(agent)}`,
        null,
        "refresh",
      );
    case "thread.cloud_handoff_started":
      return transition(activity, "info", `Continuing in ${labelAgent(agent)} Cloud`, detailText(payload), "cloud");
    case "thread.cloud_handoff_failed":
      return transition(activity, "danger", "Cloud handoff failed", detailText(payload), "alert");
    case "thread.cloud_reclaimed":
      return transition(activity, "info", "Cloud run reclaimed", detailText(payload), "cloud");
    default:
      return null;
  }
}

function localProviderLabel(payload: Record<string, unknown>): string | null {
  const provider = payload.provider;
  if (provider === "ollama") return "Ollama";
  if (provider === "lm_studio") return "LM Studio";
  return null;
}

function transition(
  activity: ActivityEvent,
  tone: "info" | "warning" | "danger",
  text: string,
  detail: string | null,
  icon?: TransitionIcon,
): TransitionItem {
  return { type: "transition", id: `activity-${activity.id}`, tone, text, detail, icon };
}

function detailText(payload: Record<string, unknown>): string | null {
  for (const key of ["detail", "url", "reason", "branch"]) {
    const value = payload[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function isLimitActivity(kind: string): boolean {
  return (
    kind === "thread.agent_limited" ||
    kind === "thread.fallback_started" ||
    kind === "thread.fallback_waiting" ||
    kind === "thread.fallback_disabled"
  );
}

// Ranks for items that have no useful timestamp of their own. They sit far above
// any real epoch value, so they always trail the conversation in this order:
// queued turns, then the message the user just sent, then any notice about it.
const QUEUED_RANK = 1e15;
const PENDING_RANK = 2e15;
const TRAILING_RANK = 3e15;

function itemTs(
  item: TranscriptItem,
  input: {
    activities: ActivityEvent[];
    cloudRuns: CloudRun[];
  },
  anchors: Map<string, number>,
): number {
  if (item.type === "event") return Date.parse(item.event.ts) || 0;
  if (item.type === "queued") return QUEUED_RANK;
  if (item.type === "pending") return PENDING_RANK;
  const anchored = anchors.get(item.id);
  if (anchored !== undefined) return anchored;
  const activity = input.activities.find((entry) => `activity-${entry.id}` === item.id);
  if (activity) return Date.parse(activity.ts) || 0;
  const cloud = input.cloudRuns.find((run) => `cloud-active-${run.id}` === item.id);
  if (cloud) return Date.parse(cloud.last_activity_at ?? cloud.launched_at) || 0;
  return Number.MAX_SAFE_INTEGER;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function eventClientMessageId(event: AgentThreadEvent): string | null {
  if (event.client_message_id) return event.client_message_id;
  const data = asRecord(event.data);
  const id = data.client_message_id;
  return typeof id === "string" && id.trim() ? id.trim() : null;
}

function parseAgent(value: unknown): AgentKind | null {
  return value === "codex" || value === "claude_code" ? value : null;
}

function labelAgent(agent: AgentKind | null | undefined): string {
  if (agent === "codex") return "Codex";
  if (agent === "claude_code") return "Claude";
  return "Agent";
}

function formatReset(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function isActiveCloudRun(status: CloudRun["status"]): boolean {
  return status === "provisioning" || status === "running" || status === "stalled";
}
