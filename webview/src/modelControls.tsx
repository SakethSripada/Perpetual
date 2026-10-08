import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import * as Popover from "@radix-ui/react-popover";
import type { AgentKind, WorkbenchSnapshot } from "./types";
import { Icon, ProviderLogo } from "./icons";
import { resourceState, ResourceState } from "./loading";
import { resolveModelSelection, effortLabel } from "./modelSelection";

type Props = {
  agent: AgentKind; snapshot: WorkbenchSnapshot | null; model: string; reasoning: string;
  options: { value: string; label: string; source: string }[];
  onAgent(agent: AgentKind): void; onModel(model: string): void; onReasoning(reasoning: string): void;
};

export function ModelControls(props: Props) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<"effort" | "models">("effort");
  const [query, setQuery] = useState("");
  const modelButton = useRef<HTMLButtonElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const selection = resolveModelSelection(props.snapshot, props.agent, props.model, props.reasoning);
  const catalog = props.snapshot?.modelCatalog?.find((item) => item.agent === props.agent);
  const state = resourceState(props.snapshot ? { ...props.snapshot, modelCatalog: catalog ? [catalog] : [] } : null, "models");
  const options = props.options.filter((option) => option.source !== "Built-in fallback" && (!/ alias$/.test(option.source) || option.value === selection.model));
  const matches = options.filter((option) => `${option.label} ${option.value}`.toLowerCase().includes(query.trim().toLowerCase()));
  const searchable = options.length > 6;
  const defaultEffort = resolveModelSelection(props.snapshot, props.agent, props.model).reasoning;

  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => {
      if (view === "models") {
        if (searchable) search.current?.focus();
        else (list.current?.querySelector('[aria-checked="true"]') as HTMLElement | null ?? list.current?.querySelector<HTMLButtonElement>('button:not(:disabled)'))?.focus();
      } else (modelButton.current ?? content.current?.querySelector<HTMLButtonElement>('button[aria-pressed="true"]'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [open, view, props.agent, searchable]);

  const switchProvider = (agent: AgentKind) => { props.onAgent(agent); setQuery(""); };
  const back = () => { setView("effort"); setQuery(""); };
  const choose = (model: string) => { if (model !== props.model) props.onModel(model); back(); };
  const moveFocus = (key: string) => {
    const buttons = Array.from(list.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? []);
    if (!buttons.length) return;
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = key === "Home" ? 0 : key === "End" ? buttons.length - 1 : index < 0 ? (key === "ArrowUp" ? buttons.length - 1 : 0) : (index + (key === "ArrowUp" ? -1 : 1) + buttons.length) % buttons.length;
    buttons[next].focus();
  };

  return <Popover.Root open={open} onOpenChange={(next) => { setOpen(next); if (!next) back(); }}>
    <Popover.Trigger asChild>
      <button type="button" className="chip-btn model-choice-trigger" aria-label="Choose model and reasoning" title={`${selection.modelLabel}${selection.reasoning ? ` · ${selection.reasoningLabel}` : ""}`}>
        <ProviderLogo agent={props.agent} /><span className="model-choice-label">{state === "loading" && !selection.model ? "Loading models" : selection.modelLabel.replace(/^Claude\s+/, "")}</span>
        {selection.reasoning && <small>{selection.reasoningLabel}</small>}<Icon name="caret" />
      </button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content ref={content} className={`model-studio model-studio-${view}`} aria-label="Model and reasoning" side="top" align="start" sideOffset={8} collisionPadding={12}
        onOpenAutoFocus={(event) => event.preventDefault()} onEscapeKeyDown={(event) => { if (view === "models") { event.preventDefault(); back(); } }}>
        <div className="model-studio-providers" aria-label="Provider">{(["codex", "claude_code"] as const).map((agent) => <button key={agent} type="button" aria-pressed={agent === props.agent} onClick={() => switchProvider(agent)}><ProviderLogo agent={agent} />{agent === "codex" ? "Codex" : "Claude Code"}</button>)}</div>
        {view === "effort" ? <>
          {(state === "ready" || selection.model) && <button ref={modelButton} type="button" className="model-studio-current" aria-label="Choose model" onClick={() => setView("models")}>
            <span><small>Model</small><strong>{state === "loading" && !selection.model ? "Loading models" : selection.modelLabel}</strong></span><Icon name="caret" />
          </button>}
          {state !== "ready" ? <ResourceState state={state} label="models" /> : selection.efforts.length ? <EffortControl key={`${props.agent}:${selection.model}`} efforts={selection.efforts} value={selection.reasoning} defaultValue={defaultEffort} onChange={props.onReasoning} /> : <div className="model-studio-no-effort"><Icon name="bolt" /><span>{selection.knownModel ? "Reasoning not adjustable" : "Reasoning unavailable"}</span></div>}
          {selection.unavailable && <p className="inline-error">Choose an available model.</p>}
        </> : <>
          <div className="model-studio-heading"><button type="button" className="quiet-icon model-studio-back" aria-label="Back to reasoning" onClick={back}><Icon name="caret" /></button><strong>Models</strong></div>
          {searchable && <div className="model-studio-search"><Icon name="search" /><input ref={search} aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); moveFocus(event.key); } }} /></div>}
          <div ref={list} className="model-studio-list" role="menu" aria-label="Models" onKeyDown={(event) => { if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) { event.preventDefault(); moveFocus(event.key); } }}>
            {state !== "ready" ? <ResourceState state={state} label="models" /> : matches.map((option, index) => <button type="button" key={option.value} tabIndex={option.value === selection.model || (index === 0 && !matches.some((item) => item.value === selection.model)) ? 0 : -1} role="menuitemradio" aria-checked={option.value === selection.model} disabled={catalog?.models.find((model) => model.id === option.value)?.available === false} onClick={() => choose(option.value)}><span>{option.label}</span>{option.value === selection.model && <Icon name="check" />}</button>)}
            {state === "ready" && !matches.length && <p className="menu-empty">No matching models</p>}
          </div>
        </>}
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}

function EffortControl({ efforts, value, defaultValue, onChange }: { efforts: string[]; value: string; defaultValue: string; onChange(value: string): void }) {
  const [draft, setDraft] = useState(value);
  const dragging = useRef(false);
  const committed = useRef(value);
  useEffect(() => { if (!dragging.current) setDraft(value); committed.current = value; }, [value]);
  const index = Math.max(0, efforts.indexOf(draft));
  const last = efforts.length - 1;
  const commit = (next: string) => { if (next !== committed.current) { committed.current = next; onChange(next); } };
  const setLevel = (next: string) => { setDraft(next); commit(next); };
  const finish = () => { dragging.current = false; if (efforts.includes(draft)) commit(draft); };
  const finishRange = (index: number) => { dragging.current = false; setLevel(efforts[index]); };
  return <div className="effort-control">
    <div className="effort-control-heading"><span><Icon name="bolt" />Reasoning</span><strong>{draft ? effortLabel(draft) : "Choose level"}</strong><button type="button" className="quiet-icon" aria-label="Reset reasoning" title={`Reset to ${effortLabel(defaultValue)}`} disabled={!defaultValue || draft === defaultValue} onClick={() => setLevel(defaultValue)}><Icon name="refresh" /></button></div>
    {last > 0 ? <>
      <div className="effort-rail" data-unselected={!efforts.includes(draft)} style={{ "--effort-fill": `${index / last * 100}%` } as CSSProperties}>
        <div className="effort-rail-track" aria-hidden="true"><span />{efforts.map((effort, position) => <i key={effort} style={{ left: `${position / last * 100}%` }} />)}</div>
        <input type="range" aria-label="Reasoning level" aria-valuetext={draft ? effortLabel(draft) : "Not selected"} min={0} max={last} step={1} value={index} onChange={(event) => setDraft(efforts[Number(event.target.value)])} onPointerDown={() => { dragging.current = true; }} onPointerUp={(event) => finishRange(Number(event.currentTarget.value))} onPointerCancel={() => { dragging.current = false; setDraft(value); }} onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) finishRange(Number(event.currentTarget.value)); }} onBlur={finish} />
      </div>
      <div className="effort-rail-labels">{efforts.map((effort) => <button type="button" key={effort} aria-pressed={effort === draft} onClick={() => setLevel(effort)}>{effortLabel(effort)}</button>)}</div>
    </> : <button type="button" className="effort-single" aria-pressed={draft === efforts[0]} onClick={() => setLevel(efforts[0])}>{effortLabel(efforts[0])}</button>}
  </div>;
}
