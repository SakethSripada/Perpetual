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
      const text = (event.text ?? "").trim();
      if (text && noAssistantUserTexts.has(text)) continue;
      if (text) noAssistantUserTexts.add(text);
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
