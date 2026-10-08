import type { AgentKind, WorkbenchSnapshot } from "./types";

export type ModelSelection = { model: string; reasoning: string };
export type ModelSelections = Partial<Record<AgentKind, ModelSelection>>;
const same = (a: string, b: string) => a.trim().replace(/\[[^\]]*\]$/, "").trim().toLowerCase() === b.trim().replace(/\[[^\]]*\]$/, "").trim().toLowerCase();

export function readModelSelections(state: any): ModelSelections {
  const selected: ModelSelections = {};
  for (const agent of ["codex", "claude_code"] as const) {
    const value = state?.modelSelections?.[agent];
    if (value && typeof value.model === "string" && typeof value.reasoning === "string") selected[agent] = {model: value.model.trim(), reasoning: value.reasoning.trim()};
  }
  if (state?.modelSelections == null && !selected[state?.lastAgent as AgentKind] && ["codex", "claude_code"].includes(state?.lastAgent) && typeof state?.lastModel === "string" && state.lastModel.trim()) {
    selected[state.lastAgent as AgentKind] = {model: state.lastModel.trim(), reasoning: typeof state.lastReasoning === "string" ? state.lastReasoning.trim() : ""};
  }
  return selected;
}

export function resolveModelSelection(snapshot: WorkbenchSnapshot | null, agent: AgentKind, model = "", reasoning = "") {
  const catalog = snapshot?.modelCatalog?.find((item) => item.agent === agent);
  const profile = snapshot?.limitPolicy?.agent_profiles?.find((item) => item.agent === agent);
  const cli = snapshot?.runDefaults.find((item) => item.kind === agent);
  const effectiveModel = model.trim() || profile?.model?.trim() || cli?.model?.trim() || catalog?.default_model?.trim() || catalog?.models.find((item) => item.default && item.available)?.id || "";
  const selected = catalog?.models.find((item) => same(item.id, effectiveModel) || item.aliases?.some((alias) => same(alias, effectiveModel)));
  const efforts = selected ? selected.reasoning : catalog?.reasoning ?? [];
  const defaultEffort = selected?.default_reasoning ?? catalog?.default_reasoning ?? profile?.reasoning ?? cli?.reasoning ?? "";
  const requestedEffort = reasoning.trim() || defaultEffort;
  const effectiveEffort = efforts.find((effort) => same(effort, requestedEffort)) ?? (selected ? (efforts.find((effort) => same(effort, defaultEffort)) ?? "") : requestedEffort);
  return {model: effectiveModel, reasoning: effectiveEffort, modelLabel: selected?.label || effectiveModel || "Choose model", reasoningLabel: effectiveEffort ? effortLabel(effectiveEffort) : "No reasoning levels", efforts, knownModel: !!selected, unavailable: selected?.available === false};
}
export function effortLabel(value: string) {
  if (value.toLowerCase() === "xhigh") return "Extra high";
  return value.replace(/[_-]+/g, " ").replace(/^\w/, (letter) => letter.toUpperCase());
}
