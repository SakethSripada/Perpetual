import { useCallback, useEffect, useRef, useState } from "react";
import { Icon } from "./icons";
import { ActionMenu, MenuItem, MenuLabel } from "./controls";

export type Notification = { id: number; message: string; error: boolean; dismissed: boolean };
export function appendNotification(items: Notification[], message: string, error: boolean, id: number) {
  // Host snapshots and action acknowledgements can report the same failure.
  if (items.some((item) => item.message === message && item.error === error && !item.dismissed)) return items;
  const next = [{ id, message, error, dismissed: false }, ...items];
  while (next.length > 12) {
    let remove = next.length - 1;
    for (let index = next.length - 1; index >= 0; index--) {
      if (!next[index].error || next[index].dismissed) { remove = index; break; }
    }
    next.splice(remove, 1);
  }
  return next;
}
export function useNotifications() {
  const [items, setItems] = useState<Notification[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const sequence = useRef(0);
  const notify = useCallback((message: string | null, error = false) => {
    if (!message) { setStatus(null); return; }
    const id = ++sequence.current;
    setItems((previous) => appendNotification(previous, message, error, id));
    if (!error) setStatus(message);
  }, []);
  useEffect(() => {
    if (!status) return;
    const timer = window.setTimeout(() => setStatus(null), 4_000);
    return () => window.clearTimeout(timer);
  }, [status]);
  const dismiss = (id: number) => setItems((previous) => previous.map((item) => item.id === id ? { ...item, dismissed: true } : item));
  return { notify, items, status, dismiss, clear: () => setItems([]) };
}

export function NotificationCenter({ state }: { state: ReturnType<typeof useNotifications> }) {
  const errors = state.items.filter((item) => item.error && !item.dismissed).length;
  return <ActionMenu label="Activity notifications" className="notification-menu" trigger={<button className="icon-btn notification-trigger" title="Notifications" aria-label={errors ? `Notifications, ${errors} unresolved ${errors === 1 ? "error" : "errors"}` : "Notifications"}><Icon name="inbox" />{errors > 0 && <span className="notification-count">{errors}</span>}</button>}>
    <MenuLabel>Recent activity</MenuLabel>
    {!state.items.length && <p className="notification-empty">You’re up to date.</p>}
    {state.items.map((item) => <div className="notification-entry" key={item.id}><Icon name={item.error ? "alert" : "check"} /><span>{item.message}</span>{item.error && !item.dismissed && <button className="quiet-icon" aria-label="Dismiss error" title="Dismiss error" onClick={() => state.dismiss(item.id)}><Icon name="close" /></button>}</div>)}
    {state.items.length > 0 && <MenuItem onSelect={state.clear}>Clear history</MenuItem>}
  </ActionMenu>;
}
export function ErrorStatus({ state, onRefresh }: { state: ReturnType<typeof useNotifications>; onRefresh(): void }) {
  const error = state.items.find((item) => item.error && !item.dismissed);
  if (!error) return null;
  return <div className="error-status" role="alert"><Icon name="alert" /><span>{error.message}</span><button className="text-btn" onClick={onRefresh}>Refresh status</button><button className="quiet-icon" title="Dismiss error" aria-label="Dismiss error" onClick={() => state.dismiss(error.id)}><Icon name="close" /></button></div>;
}
