import type { SessionSummary } from "./api";

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
