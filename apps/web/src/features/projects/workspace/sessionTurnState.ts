export interface SessionTurnCandidates {
  readonly active: string | undefined;
  readonly pending: string | undefined;
  readonly latest: string | undefined;
}

export interface AcceptedSessionTurn {
  readonly id: string;
  readonly route: string;
}

/** Keep the rendered turn stable while session queries are being invalidated. */
export function stableSessionTurnId(
  previous: string | undefined,
  candidates: SessionTurnCandidates,
): string | undefined {
  return candidates.active ?? candidates.pending ?? previous ?? candidates.latest;
}

/** A started or handed-off POST response is authoritative before session SSE
 * catches up. Queued messages must leave the currently running turn visible. */
export function renderSessionTurnId(
  previous: string | undefined,
  candidates: SessionTurnCandidates,
  accepted: AcceptedSessionTurn | null,
): string | undefined {
  if (accepted && (accepted.route === "started" || accepted.route === "handed_off")) {
    return accepted.id;
  }
  if (accepted?.route === "queued" && previous) {
    return candidates.active ?? previous;
  }
  return stableSessionTurnId(previous, candidates);
}
