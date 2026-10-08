import { useEffect, useRef, useState } from "react";
import type { AgentThread, TaskStatus, WorkbenchSnapshot } from "./types";
import { Icon } from "./icons";
import { ActionMenu, ContextActions, RowActions, type RowAction } from "./controls";
import { request } from "./bridge";
import { useSheetAccessibility } from "./dialogs";
import { resourceState, ResourceState } from "./loading";
export const statusLabel = (status: TaskStatus) => ({ draft: "Draft", queued: "Queued", running: "Working", running_in_cloud: "Working in cloud", awaiting_approval: "Needs approval", waiting_for_limit: "Waiting for a reset", waiting_for_network: "Waiting for network", paused: "Paused", review: "Finished", done: "Done", failed: "Failed", cancelled: "Stopped" })[status];

export function searchSessions(threads: AgentThread[], query: string) {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  return threads.filter((thread) => terms.every((term) => `${thread.title} ${thread.objective ?? ""}`.toLocaleLowerCase().includes(term)));
}
type Props = { snapshot: WorkbenchSnapshot | null; selectedId?: string | null; onSelect(id: string): void; onReview?(id: string): void; onDelete(id: string, force: boolean): void | Promise<void> };
export function SessionCollection(props: Props & { autofocus?: boolean }) {
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<AgentThread | null>(null);
  const [name, setName] = useState("");
  const [remove, setRemove] = useState<AgentThread | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef(false);
  const search = useRef<HTMLInputElement>(null);
  const rows = useRef<(HTMLButtonElement | null)[]>([]);
  const dragged = useRef<string | null>(null);
  const threads = props.snapshot?.threads ?? [];
  const filtered = searchSessions(threads, query);
  const historyState = resourceState(props.snapshot, "threads");
  useEffect(() => { if (props.autofocus) search.current?.focus(); }, [props.autofocus]);
  const run = async (action: () => Promise<unknown>, done?: () => void) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { await action(); done?.(); } catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const move = (source: string, target: string) => {
    if (source === target || query.trim()) return;
    const orderedIds = threads.map((thread) => thread.id);
    const from = orderedIds.indexOf(source), to = orderedIds.indexOf(target);
    if (from < 0 || to < 0) return;
    orderedIds.splice(from, 1); orderedIds.splice(to, 0, source);
    void run(() => request({ type: "reorderThreads", orderedIds }));
  };
  const actions = (thread: AgentThread): RowAction[] => {
    const index = threads.findIndex((item) => item.id === thread.id);
    return [
      { label: "Open", onSelect: () => props.onSelect(thread.id) },
      { label: "Rename", disabled: busy, onSelect: () => { setEditing(thread); setName(thread.title); } },
      ...(props.onReview ? [{ label: "Changes", icon: <Icon name="repo" />, onSelect: () => props.onReview?.(thread.id) }] : []),
      ...(["running", "running_in_cloud", "awaiting_approval", "queued"].includes(thread.status) ? [{ label: "Stop run", icon: <Icon name="stop" />, disabled: busy, onSelect: () => { void run(() => request({ type: "stopThread", threadId: thread.id })); } }] : []),
      { label: "Move up", separated: true, icon: <Icon name="up" />, disabled: busy || !!query.trim() || index === 0, onSelect: () => move(thread.id, threads[index - 1].id) },
      { label: "Move down", icon: <Icon name="down" />, disabled: busy || !!query.trim() || index === threads.length - 1, onSelect: () => move(thread.id, threads[index + 1].id) },
      { label: "Delete", separated: true, icon: <Icon name="trash" />, danger: true, disabled: busy, onSelect: () => setRemove(thread) },
    ];
  };
  return <div className="session-collection" aria-busy={busy}>
    <div className="session-search"><Icon name="search" /><input ref={search} data-initial-focus={props.autofocus || undefined} type="search" aria-label="Search conversations" placeholder="Search chats" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "ArrowDown") { event.preventDefault(); rows.current[0]?.focus(); } if (event.key === "Enter" && filtered[0]) props.onSelect(filtered[0].id); }} />{query && <button className="quiet-icon" aria-label="Clear search" onClick={() => { setQuery(""); search.current?.focus(); }}><Icon name="close" /></button>}</div>
    <div className="sidebar-section-label" role="status">{query.trim() ? `${filtered.length} ${filtered.length === 1 ? "result" : "results"}` : "Chats"}</div>
    <div className="sidebar-sessions" role="list" aria-label="Conversations">
      {historyState !== "ready" && <ResourceState state={historyState} label="chats" />}
      {historyState === "ready" && !filtered.length && <div className="session-empty"><span>{query ? "No matches" : "No chats yet"}</span></div>}
      {filtered.map((thread, index) => <ContextActions actions={actions(thread)} key={thread.id}><div role="listitem" className={`session-row${props.selectedId === thread.id ? " selected" : ""}`} draggable={!query.trim() && !busy} onDragStart={() => { dragged.current = thread.id; }} onDragEnd={() => { dragged.current = null; }} onDragOver={(event) => event.preventDefault()} onDrop={() => { if (dragged.current) move(dragged.current, thread.id); dragged.current = null; }}>
        <button ref={(element) => { rows.current[index] = element; }} className="session-select" aria-current={props.selectedId === thread.id ? "page" : undefined} title={`${thread.title} · ${statusLabel(thread.status)} · ${new Date(thread.updated_at).toLocaleDateString()}`} onClick={() => props.onSelect(thread.id)} onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); const next = index + (event.key === "ArrowDown" ? 1 : -1); if (next < 0) search.current?.focus(); else rows.current[Math.min(next, filtered.length - 1)]?.focus(); } }}><span className="session-copy"><strong>{thread.title || "Untitled chat"}</strong>{["running", "queued", "awaiting_approval", "waiting_for_limit", "failed"].includes(thread.status) && <small>{statusLabel(thread.status)}</small>}</span></button>
        <ActionMenu label={`Actions for ${thread.title}`} trigger={<button className="session-remove quiet-icon" title={`Options for ${thread.title}`} aria-label={`Options for ${thread.title}`}><Icon name="more" /></button>}><RowActions actions={actions(thread)} /></ActionMenu>
      </div></ContextActions>)}
    </div>
    {editing && <form className="session-edit" onSubmit={(event) => { event.preventDefault(); if (name.trim()) void run(() => request({ type: "renameThread", threadId: editing.id, title: name.trim() }), () => { setEditing(null); search.current?.focus(); }); }}><label>Rename conversation<input autoFocus aria-label="Conversation name" maxLength={200} value={name} disabled={busy} onChange={(event) => setName(event.target.value)} /></label><div><button className="secondary-btn" type="button" disabled={busy} onClick={() => { setEditing(null); search.current?.focus(); }}>Cancel</button><button className="primary-btn" disabled={busy || !name.trim()}>Save name</button></div></form>}
    {remove && <div className="session-edit" role="alertdialog" aria-label="Delete conversation"><strong>Delete “{remove.title}”?</strong><p>This removes conversation history and its managed worktrees.</p><div><button className="secondary-btn" disabled={busy} onClick={() => { setRemove(null); search.current?.focus(); }}>Cancel</button><button className="primary-btn danger" disabled={busy} onClick={() => void run(async () => { await props.onDelete(remove.id, true); }, () => { setRemove(null); search.current?.focus(); })}>{busy ? "Deleting…" : "Delete conversation"}</button></div></div>}
    {error && <p className="inline-error" role="alert">{error}</p>}
  </div>;
}
export function SessionHistory(props: Props & { onClose(): void; onNew(): void }) {
  useSheetAccessibility(true);
  return <div className="sheet-backdrop" onClick={props.onClose}><section className="sheet session-history-sheet" role="dialog" aria-modal="true" aria-label="Conversations" onClick={(event) => event.stopPropagation()}><header><strong>Conversations</strong><button className="icon-btn" title="Close" aria-label="Close" onClick={props.onClose}><Icon name="close" /></button></header><SessionCollection {...props} autofocus /><footer><button className="secondary-btn" onClick={props.onNew}><Icon name="plus" />New conversation</button></footer></section></div>;
}
