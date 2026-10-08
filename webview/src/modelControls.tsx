import { useState } from "react";
import type { AgentKind, WorkbenchSnapshot } from "./types";
import { ActionMenu, MenuItem, MenuSeparator } from "./controls";
import { Icon, ProviderLogo } from "./icons";
import { resourceState, ResourceState } from "./loading";
import { resolveModelSelection, effortLabel } from "./modelSelection";

export function ModelControls(props: {
  agent: AgentKind; snapshot: WorkbenchSnapshot | null; model: string; reasoning: string;
  options: {value: string; label: string; source: string}[];
  onAgent(agent: AgentKind): void; onModel(model: string): void; onReasoning(reasoning: string): void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const selection = resolveModelSelection(props.snapshot, props.agent, props.model, props.reasoning);
  const state = resourceState(props.snapshot, "models");
  const matches = props.options.filter((option) => !/ alias$/.test(option.source) || option.value === selection.model).filter((option) => `${option.label} ${option.value}`.toLowerCase().includes(query.trim().toLowerCase()));
  return <ActionMenu label="Model and reasoning" open={open} onOpenChange={(next) => { setOpen(next); if (!next) setQuery(""); }} side="top" align="start" className="model-choice-menu" trigger={<button type="button" className="chip-btn model-choice-trigger" aria-label="Choose model and reasoning" title={`${selection.modelLabel}${selection.reasoning ? ` · ${selection.reasoningLabel}` : ""}`}><ProviderLogo agent={props.agent} /><span className="model-choice-label">{state === "loading" && !selection.model ? "Loading models" : selection.modelLabel}</span>{selection.reasoning && <small>{selection.reasoningLabel}</small>}<Icon name="caret" /></button>}>
    <div className="model-provider-tabs" aria-label="Provider">{(["codex", "claude_code"] as const).map((agent) => <button key={agent} type="button" aria-pressed={agent === props.agent} onClick={() => { props.onAgent(agent); setQuery(""); }}><ProviderLogo agent={agent} />{agent === "codex" ? "Codex" : "Claude Code"}</button>)}</div>
    <div className="choice-search"><Icon name="search" /><input aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (!["Escape", "Tab", "ArrowDown", "ArrowUp"].includes(event.key)) event.stopPropagation(); }} /></div>
    <div className="model-choice-list">{state !== "ready" ? <ResourceState state={state} label="models" /> : matches.map((option) => <MenuItem key={option.value} role="menuitemradio" aria-checked={option.value === selection.model} disabled={props.snapshot?.modelCatalog?.find((catalog) => catalog.agent === props.agent)?.models.find((model) => model.id === option.value)?.available === false} onSelect={(event) => { event.preventDefault(); if (option.value !== props.model) props.onModel(option.value); }}><span>{option.label}</span>{option.value === selection.model && <Icon name="check" />}</MenuItem>)}{state === "ready" && !matches.length && <p className="menu-empty">No matching models</p>}</div>
    {selection.unavailable && <p className="inline-error">Choose an available model.</p>}
    <MenuSeparator />
    <div className="model-reasoning-heading"><span>Reasoning</span><small>{selection.efforts.length ? selection.reasoningLabel : selection.knownModel ? "Not supported by this model" : "Not reported by provider"}</small></div>
    {selection.efforts.length > 0 && <div className="model-effort-options">{selection.efforts.map((effort) => <button key={effort} type="button" aria-pressed={effort === selection.reasoning} onClick={() => props.onReasoning(effort)}>{effortLabel(effort)}</button>)}</div>}
  </ActionMenu>;
}
