import { useRef, useState } from "react";
import type { AgentThread, TaskStatus, WorkbenchSnapshot } from "./types";
import { BrandMark, Icon } from "./icons";
import { AccountSwitcher } from "./accounts";
import { request } from "./bridge";

export const statusLabel = (status: TaskStatus) => ({ draft: "Draft", queued: "Queued", running: "Working", running_in_cloud: "Working in cloud", awaiting_approval: "Needs approval", waiting_for_limit: "Waiting for a reset", waiting_for_network: "Waiting for network", paused: "Paused", review: "Finished", done: "Done", failed: "Failed", cancelled: "Stopped" })[status];

export function SessionSidebar(props: { snapshot: WorkbenchSnapshot | null; selected: AgentThread | null; agent: "codex" | "claude_code"; onNew(): void; onSelect(id: string): void; onDelete(id: string, force: boolean): void; onAccounts(): void; onAgent(agent: "codex" | "claude_code"): void; onSettings(): void; onReview(id: string): void }) {
  const dragged = useRef<string | null>(null);
  const [optionsId, setOptionsId] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<AgentThread | null>(null);
  const [name, setName] = useState("");
  const [query, setQuery] = useState("");
  const [remove, setRemove] = useState<AgentThread | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const threads = props.snapshot?.threads ?? [];
  const filtered = threads.filter((thread) => thread.title.toLowerCase().includes(query.toLowerCase()));
  const reorder = async (target: string) => {
    const source = dragged.current;
    if (!source || source === target || busy || query) return;
    const orderedIds = threads.map((thread) => thread.id);
    const from = orderedIds.indexOf(source), to = orderedIds.indexOf(target);
    orderedIds.splice(from, 1); orderedIds.splice(to, 0, source);
    setBusy(true); setDragging(null);
    try { await request({ type: "reorderThreads", orderedIds }); }
    catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  };
  return <aside className="session-sidebar" aria-label="Sessions">
    <div className="sidebar-brand"><BrandMark size={24} /><strong>Perpetual</strong><button className="icon-btn" title="Settings" aria-label="Settings" onClick={props.onSettings}><Icon name="settings" /></button></div>
    <button className="new-session-button" onClick={props.onNew}><Icon name="plus" />New session<span>Ctrl Alt N</span></button>
    <label className="session-search"><Icon name="search" /><input aria-label="Search sessions" placeholder="Search sessions" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
    <div className="sidebar-section-label">Sessions <span>{threads.length}</span></div>
    <div className="sidebar-sessions">
      {!props.snapshot && <p className="sidebar-empty">Connecting…</p>}
      {props.snapshot && !filtered.length && <p className="sidebar-empty">{query ? "No matching sessions" : "Your conversations will appear here."}</p>}
      {filtered.map((thread) => <div className={`session-row${props.selected?.id === thread.id ? " selected" : ""}${dragging === thread.id ? " dragging" : ""}`} key={thread.id} draggable={!query && !busy} onDragStart={() => { dragged.current = thread.id; setDragging(thread.id); }} onDragEnd={() => { dragged.current = null; setDragging(null); }} onDragOver={(event) => event.preventDefault()} onDrop={() => void reorder(thread.id)}>
        <button className="session-select" onDoubleClick={() => { setRenaming(thread); setName(thread.title); }} aria-current={props.selected?.id === thread.id ? "page" : undefined} onClick={() => props.onSelect(thread.id)}><span className={`session-dot ${thread.status}`} /><span><strong>{thread.title}</strong><small>{statusLabel(thread.status)}</small></span></button>
        <button className="session-remove" title={`Options for ${thread.title}`} aria-label={`Options for ${thread.title}`} aria-expanded={optionsId === thread.id} onClick={() => setOptionsId(optionsId === thread.id ? null : thread.id)}><Icon name="sliders" /></button>
        {optionsId === thread.id && <div className="session-options">
          <button onClick={() => { setRenaming(thread); setName(thread.title); setOptionsId(null); }}>Rename</button>
          <button onClick={() => { props.onReview(thread.id); setOptionsId(null); }}>Review changes</button>
          {["running", "awaiting_approval", "queued"].includes(thread.status) && <button disabled={busy} onClick={() => void request({ type: "stopThread", threadId: thread.id }).catch((error) => setError(String(error)))}>Stop run</button>}
          <button disabled={busy || threads.indexOf(thread) === 0} onClick={() => { dragged.current = thread.id; void reorder(threads[threads.indexOf(thread) - 1].id); }}>Move up</button>
          <button disabled={busy || threads.indexOf(thread) === threads.length - 1} onClick={() => { dragged.current = thread.id; void reorder(threads[threads.indexOf(thread) + 1].id); }}>Move down</button>
          <button className="danger-text" onClick={() => { setRemove(thread); setOptionsId(null); }}>Delete session</button>
        </div>}
      </div>)}
    </div>
    {renaming && <form className="sidebar-confirm" onSubmit={async (event) => { event.preventDefault(); if (busy || !name.trim()) return; setBusy(true); try { await request({ type: "renameThread", threadId: renaming.id, title: name.trim() }); setRenaming(null); } catch (error) { setError(String(error)); } finally { setBusy(false); } }}><label>Rename session<input autoFocus aria-label="Session name" maxLength={200} value={name} onChange={(event) => setName(event.target.value)} /></label><button className="secondary-btn" type="button" disabled={busy} onClick={() => setRenaming(null)}>Cancel</button><button className="secondary-btn" disabled={busy || !name.trim()}>Save</button></form>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    {remove && <div className="sidebar-confirm" role="alertdialog" aria-label="Delete session"><strong>Delete “{remove.title}”?</strong><p>Conversation history and managed worktrees will be removed.</p><button className="secondary-btn" onClick={() => setRemove(null)}>Cancel</button><button className="secondary-btn danger-text" onClick={() => { props.onDelete(remove.id, true); setRemove(null); }}>Delete</button></div>}
    <div className="sidebar-footer"><AccountSwitcher snapshot={props.snapshot} agent={props.agent} onManage={props.onAccounts} onPickAgent={props.onAgent} /><button className="text-btn" onClick={props.onAccounts}>Manage accounts</button></div>
  </aside>;
}

