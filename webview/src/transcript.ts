import type {
  ActivityEvent,
  AgentKind,
  AgentThread,
  AgentThreadEvent,
  CloudRun,
  QueuedTurn,
} from "./types";

export type TranscriptItem =
  | { type: "event"; event: AgentThreadEvent }
  | { type: "transition"; id: string; tone: "info" | "warning" | "danger"; text: string; detail?: string | null }
  | { type: "queued"; id: string; message: string };

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

  for (const activity of input.activities) {
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
      : `Waiting for ${labelAgent(original)} reset`;
    items.push({
      type: "transition",
      id: `usage-limit-${input.thread.id}`,
      tone: fallback ? "warning" : "danger",
      text,
      detail: input.thread.limit_reset_at ? `Reset ${formatReset(input.thread.limit_reset_at)}` : null,
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
    });
  }

  for (const queued of input.queued) {
    if (queued.echo_user_message === false) continue;
    items.push({ type: "queued", id: queued.id, message: queued.message });
  }

  return items.sort((a, b) => itemTs(a, input) - itemTs(b, input));
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
  const detail = reset ? `Reset ${formatReset(reset)}` : detailText(payload);
  switch (activity.kind) {
    case "thread.agent_limited":
      return transition(activity, "warning", `${labelAgent(agent)} rate-limited`, detail);
    case "thread.fallback_started":
      return transition(
        activity,
        "warning",
        `${labelAgent(from)} rate-limited; switching to ${labelAgent(to)}`,
        detail,
      );
    case "thread.fallback_waiting":
      return transition(activity, "danger", `Waiting for ${labelAgent(agent)} reset`, detail);
    case "thread.fallback_disabled":
      return transition(activity, "danger", `${labelAgent(agent)} rate-limited; switching is off`, detail);
    case "thread.switchback_started":
      return transition(activity, "info", `${labelAgent(to)} available; switching back`, null);
    case "thread.switchback_completed":
      return transition(activity, "info", `Switched back to ${labelAgent(agent)}`, null);
    case "thread.local_fallback_started":
      return transition(activity, "warning", "Cloud unavailable; switching to local Codex", detail);
    case "thread.local_fallback_failed":
      return transition(activity, "danger", "Local fallback failed", detail);
    case "thread.cloud_handoff_started":
      return transition(activity, "info", `Continuing in ${labelAgent(agent)} Cloud`, detailText(payload));
    case "thread.cloud_handoff_failed":
      return transition(activity, "danger", "Cloud handoff failed", detailText(payload));
    case "thread.cloud_reclaimed":
      return transition(activity, "info", "Cloud run reclaimed", detailText(payload));
    default:
      return null;
  }
}

function transition(
  activity: ActivityEvent,
  tone: "info" | "warning" | "danger",
  text: string,
  detail: string | null,
): TransitionItem {
  return { type: "transition", id: `activity-${activity.id}`, tone, text, detail };
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

function itemTs(item: TranscriptItem, input: {
  activities: ActivityEvent[];
  cloudRuns: CloudRun[];
}): number {
  if (item.type === "event") return Date.parse(item.event.ts) || 0;
  if (item.type === "queued") return Number.MAX_SAFE_INTEGER - 1;
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
