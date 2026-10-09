import { useEffect, useRef, useState } from "react";
import type { CSSProperties, RefObject } from "react";
import * as Popover from "@radix-ui/react-popover";
import type { AgentKind, WorkbenchSnapshot } from "./types";
import { Icon, ProviderLogo } from "./icons";
import { resourceState, ResourceState } from "./loading";
import { resolveModelSelection, effortLabel, formatModelLabel } from "./modelSelection";

type View = "effort" | "models" | "providers";
type Props = {
  agent: AgentKind; snapshot: WorkbenchSnapshot | null; model: string; reasoning: string;
  options: { value: string; label: string; source: string }[];
  onAgent(agent: AgentKind): void; onModel(model: string): void; onReasoning(reasoning: string): void;
};
type SummaryProps = {
  agent: AgentKind; model: string; modelRef: RefObject<HTMLButtonElement>;
  onModel(): void; onProvider(): void;
};

export function ModelControls(props: Props) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<View>("effort");
  const [query, setQuery] = useState("");
  const providerReturn = useRef<View>("effort");
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
  const providerName = props.agent === "codex" ? "Codex" : "Claude Code";

  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => {
      if (view === "models" && searchable) search.current?.focus();
      else if (view !== "effort") (list.current?.querySelector('[aria-checked="true"]') as HTMLElement | null ?? list.current?.querySelector<HTMLButtonElement>('button:not(:disabled)'))?.focus();
      else (modelButton.current ?? content.current?.querySelector<HTMLButtonElement>('button'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [open, view, props.agent, searchable]);

  const back = () => { setView("effort"); setQuery(""); };
  const showProviders = () => { providerReturn.current = view; setView("providers"); };
  const switchProvider = (agent: AgentKind) => { props.onAgent(agent); setQuery(""); setView(providerReturn.current); };
  const choose = (model: string) => { if (model !== props.model) props.onModel(model); back(); };
  const moveFocus = (key: string) => {
    const buttons = Array.from(list.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? []);
    if (!buttons.length) return;
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = key === "Home" ? 0 : key === "End" ? buttons.length - 1 : index < 0 ? (key === "ArrowUp" ? buttons.length - 1 : 0) : (index + (key === "ArrowUp" ? -1 : 1) + buttons.length) % buttons.length;
    buttons[next].focus();
  };
  const summary: SummaryProps = { agent: props.agent, model: selection.modelLabel, modelRef: modelButton, onModel: () => setView("models"), onProvider: showProviders };

  return <Popover.Root open={open} onOpenChange={(next) => { setOpen(next); if (!next) back(); }}>
    <Popover.Trigger asChild>
      <button type="button" className="chip-btn model-choice-trigger" aria-label="Choose model and reasoning" title={`${selection.modelLabel}${selection.reasoning ? ` · ${selection.reasoningLabel}` : ""}`}>
        <span className="model-choice-label">{state === "loading" && !selection.model ? "Loading models" : selection.modelLabel.replace(/^Claude\s+/, "")}</span>
        {selection.reasoning && <small>{selection.reasoningLabel}</small>}<Icon name="caret" />
      </button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content ref={content} className={`model-studio model-studio-${view}`} aria-label="Model and reasoning" side="top" align="start" sideOffset={8} collisionPadding={12}
        onOpenAutoFocus={(event) => event.preventDefault()} onEscapeKeyDown={(event) => { if (view !== "effort") { event.preventDefault(); if (view === "providers") setView(providerReturn.current); else back(); } }}>
        {view === "effort" ? <>
          {state !== "ready" ? <><div className="model-studio-pending-header"><ProviderButton agent={props.agent} onClick={showProviders} /></div><ResourceState state={state} label="models" /></> : selection.efforts.length ?
            <EffortControl key={`${props.agent}:${selection.model}`} {...summary} efforts={selection.efforts} value={selection.reasoning} defaultValue={defaultEffort} onChange={props.onReasoning} /> :
            <ModelSummary {...summary} title={selection.modelLabel} subtitle={selection.knownModel ? "Reasoning not adjustable" : "Reasoning unavailable"} />}
          {selection.unavailable && <p className="inline-error">Choose an available model.</p>}
        </> : <>
          <div className="model-studio-heading"><button type="button" className="quiet-icon model-studio-back" aria-label="Back to reasoning" onClick={() => view === "providers" ? setView(providerReturn.current) : back()}><Icon name="caret" /></button><span>{view === "providers" ? "Run with" : `${providerName} models`}</span>{view === "models" && <ProviderButton agent={props.agent} onClick={showProviders} />}</div>
          {view === "models" && searchable && <div className="model-studio-search"><Icon name="search" /><input ref={search} aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); moveFocus(event.key); } }} /></div>}
          <div ref={list} className="model-studio-list" role="menu" aria-label={view === "providers" ? "Providers" : "Models"} onKeyDown={(event) => { if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) { event.preventDefault(); moveFocus(event.key); } }}>
            {view === "providers" ? (["codex", "claude_code"] as const).map((agent) => <button type="button" key={agent} role="menuitemradio" aria-checked={agent === props.agent} tabIndex={agent === props.agent ? 0 : -1} onClick={() => switchProvider(agent)}><ProviderLogo agent={agent} /><span>{agent === "codex" ? "Codex" : "Claude Code"}</span>{agent === props.agent && <Icon name="check" />}</button>) : state !== "ready" ? <ResourceState state={state} label="models" /> : matches.map((option, index) => <button type="button" key={option.value} tabIndex={option.value === selection.model || (index === 0 && !matches.some((item) => item.value === selection.model)) ? 0 : -1} role="menuitemradio" aria-checked={option.value === selection.model} disabled={catalog?.models.find((model) => model.id === option.value)?.available === false} onClick={() => choose(option.value)}><span>{formatModelLabel(option.label)}</span>{option.value === selection.model && <Icon name="check" />}</button>)}
            {view === "models" && state === "ready" && !matches.length && <p className="menu-empty">No matching models</p>}
          </div>
        </>}
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}

function ProviderButton({ agent, onClick }: { agent: AgentKind; onClick(): void }) {
  return <button type="button" className="model-provider-button" aria-label="Choose provider" title={agent === "codex" ? "Codex" : "Claude Code"} onClick={onClick}><ProviderLogo agent={agent} /><Icon name="caret" /></button>;
}

function ModelSummary(props: SummaryProps & { title: string; subtitle: string; onReset?(): void; resetDisabled?: boolean; resetTitle?: string }) {
  return <div className="model-summary">
    <ProviderButton agent={props.agent} onClick={props.onProvider} />
    <button ref={props.modelRef} type="button" className="model-summary-choice" aria-label="Choose model" onClick={props.onModel}><strong>{props.title}</strong><span><span>{props.subtitle}</span><Icon name="caret" /></span></button>
    {props.onReset ? <button type="button" className="quiet-icon model-reset" aria-label="Reset reasoning" title={props.resetTitle} disabled={props.resetDisabled} onClick={props.onReset}><Icon name="refresh" /></button> : <span className="model-summary-spacer" />}
  </div>;
}

function EffortControl(props: SummaryProps & { efforts: string[]; value: string; defaultValue: string; onChange(value: string): void }) {
  const { efforts, value, defaultValue, onChange } = props;
  const [draft, setDraft] = useState(value);
  const dragging = useRef(false);
  const [pointerMoving, setPointerMoving] = useState(false);
  const committed = useRef(value);
  useEffect(() => { if (!dragging.current) setDraft(value); committed.current = value; }, [value]);
  const index = Math.max(0, efforts.indexOf(draft));
  const last = efforts.length - 1;
  const commit = (next: string) => { if (next !== committed.current) { committed.current = next; onChange(next); } };
  const setLevel = (next: string) => { setDraft(next); commit(next); };
  const finish = () => { setPointerMoving(false); dragging.current = false; if (efforts.includes(draft)) commit(draft); };
  const finishRange = (index: number) => { setPointerMoving(false); dragging.current = false; setLevel(efforts[index]); };
  return <>
    <ModelSummary {...props} title={draft ? effortLabel(draft) : "Choose effort"} subtitle={props.model} onReset={() => setLevel(defaultValue)} resetDisabled={!defaultValue || draft === defaultValue} resetTitle={`Reset to ${effortLabel(defaultValue)}`} />
    <div className="effort-control">
      {last > 0 ? <div className="effort-rail" data-dragging={pointerMoving} data-unselected={!efforts.includes(draft)} style={{ "--effort-fill": `${index / last * 100}%`, "--effort-offset": `${11 - 22 * index / last}px` } as CSSProperties}>
        <div className="effort-rail-track" aria-hidden="true"><span />{efforts.map((effort, position) => <i key={effort} style={{ left: `calc(${position / last * 100}% + ${11 - 22 * position / last}px)` }} />)}</div>
        <input type="range" aria-label="Reasoning level" title={draft ? effortLabel(draft) : "Choose effort"} aria-valuetext={draft ? effortLabel(draft) : "Not selected"} min={0} max={last} step={1} value={index} onChange={(event) => setDraft(efforts[Number(event.target.value)])} onPointerDown={() => { dragging.current = true; }} onPointerMove={(event) => { if (event.buttons === 1 && dragging.current) setPointerMoving(true); }} onPointerUp={(event) => finishRange(Number(event.currentTarget.value))} onPointerCancel={() => { setPointerMoving(false); dragging.current = false; setDraft(value); }} onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) finishRange(Number(event.currentTarget.value)); }} onBlur={finish} />
        <span className="effort-rail-thumb" aria-hidden="true" />

      </div> : <button type="button" className="effort-single" aria-pressed={draft === efforts[0]} onClick={() => setLevel(efforts[0])}>{effortLabel(efforts[0])}</button>}
    </div>
  </>;
}
