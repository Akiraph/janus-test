import type { TurnSummary } from "../../lib/api";

const ACTIVE_TURN_STATUSES = new Set(["running", "canceling"]);

/** Whether the composer should replace Send with Cancel for the displayed Turn. */
export function isTurnRunning(turn: Pick<TurnSummary, "status"> | null): boolean {
  return turn !== null && ACTIVE_TURN_STATUSES.has(turn.status);
}
