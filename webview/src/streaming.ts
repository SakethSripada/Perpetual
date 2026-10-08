import type { AgentThreadEvent } from './types';

/** Merge full event snapshots once per frame, preserving unaffected objects. */
export function mergeThreadEvents(
  current: AgentThreadEvent[],
  updates: AgentThreadEvent[],
): AgentThreadEvent[] {
  const pending = new Map(updates.map((event) => [event.id, event]));
  const next = current.map((event) => {
    const update = pending.get(event.id);
    pending.delete(event.id);
    // A history refresh can recover a completion missed by the live listener.
    if (
      (event.data as { streaming?: boolean } | null)?.streaming === false &&
      (update?.data as { streaming?: boolean } | null)?.streaming === true
    )
      return event;
    // Both snapshots are cumulative while streaming. A delayed history/live
    // callback must never rewind text that has already reached the screen.
    if (
      (event.data as { streaming?: boolean } | null)?.streaming === true &&
      (update?.data as { streaming?: boolean } | null)?.streaming === true &&
      (update?.text?.length ?? 0) < (event.text?.length ?? 0)
    )
      return event;
    return update ?? event;
  });
  return [...next, ...pending.values()];
}

/** Keep live updates received during a fetch, without retaining deleted rows. */
export function mergeSnapshot<T extends { id: string }>(snapshot: T[], updates: T[]): T[] {
  const byId = new Map(snapshot.map((item) => [item.id, item]));
  for (const item of updates) byId.set(item.id, item);
  return [...byId.values()];
}
