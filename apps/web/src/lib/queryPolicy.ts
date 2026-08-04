import type { SessionSummary, TimelinePage } from "./api";

/** Keep visible history during the short empty projection window that can
 * occur while a new turn is being committed. A session timeline is append
 * only for its lifetime, so an empty result after a non-empty result is not a
 * meaningful update. */
export function retainSessionTimeline(
  previous: TimelinePage | undefined,
  next: TimelinePage,
  session: Pick<SessionSummary, "state"> | undefined,
): TimelinePage {
  if (!previous?.items.length || session?.state === "deleting") return next;
  if (next.items.length === 0) return previous;

  // Timeline reads are full snapshots. Invalidations can overlap, though, and
  // a slower older response must not roll the conversation back. Merge by
  // durable item id, then keep the newest API page size worth of rows.
  const byId = new Map(previous.items.map((item) => [item.id, item]));
  for (const item of next.items) byId.set(item.id, item);
  const merged = [...byId.values()].sort((left, right) => left.display_order - right.display_order);
  const newestPrevious = previous.items.at(-1)?.display_order ?? 0;
  const newestNext = next.items.at(-1)?.display_order ?? 0;
  if (newestNext < newestPrevious) return previous;

  const items = merged.slice(-100);
  return {
    ...next,
    items,
    has_older: next.has_older || merged.length > items.length,
    oldest_cursor: items[0]?.display_order.toString() ?? null,
    newest_cursor: items.at(-1)?.display_order.toString() ?? null,
  };
}

/** Poll only while the authoritative Session owns an active Turn. SSE events
 * invalidate the query on state changes, so an idle fallback poll is wasteful.
 */
export function sessionTimelineRefetchInterval(
  itemCount: number,
  cachedSession: SessionSummary | undefined,
): number | false {
  if (itemCount === 0) return false;
  return cachedSession?.active_turn_id ? 3000 : false;
}

/** Placeholder data keeps a query mounted while its key changes. It is only
 * valid for rendering after the returned entity matches the requested key. */
export function visibleTurnData<T extends { id: string }>(
  data: T | undefined,
  turnId: string | undefined,
): T | undefined {
  return data?.id === turnId ? data : undefined;
}
