/**
 * Global event cursor manager for SSE reconnection.
 *
 * Persists the last received cursor in localStorage so that a page refresh
 * resumes from the correct position instead of replaying the full event
 * history.  Falls back to in-memory state when localStorage is unavailable
 * (private browsing, storage errors).
 */

const STORAGE_KEY = "janus:event-cursor";
export const EVENT_CURSOR_RESET = "janus:event-cursor-reset";

let memoryCursor: string | null = null;

export function getLastEventCursor(): string | null {
  if (memoryCursor !== null) return memoryCursor;
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) {
      memoryCursor = stored;
      return stored;
    }
  } catch {
    // Storage unavailable — degrade to no cursor.
  }
  return null;
}

export function setLastEventCursor(cursor: string): void {
  memoryCursor = cursor;
  try {
    localStorage.setItem(STORAGE_KEY, cursor);
  } catch {
    // Best-effort: if storage is full, the in-memory cursor still works
    // for the current session — only a full refresh loses it.
  }
}

export function clearEventCursor(): void {
  memoryCursor = null;
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Best-effort.
  }
}

/** Reconcile a persisted cursor against a known current server high-water
 * mark. A restarted or replaced local database can make a previously valid
 * browser cursor point past the new event log. */
export function reconcileEventCursor(serverCursor: string | null): string | null {
  return reconcileEventCursorBounds(null, serverCursor);
}

/** Reconcile against both retained bounds. An expired cursor is moved to just
 * before the first retained event so the next SSE connection can replay what
 * is still available instead of receiving another expiry error from `after=0`.
 */
export function reconcileEventCursorBounds(
  serverMinCursor: string | null,
  serverMaxCursor: string | null,
): string | null {
  const localCursor = getLastEventCursor();
  if (!localCursor) return null;
  if (
    (serverMinCursor !== null && !isDecimalCursor(serverMinCursor)) ||
    (serverMaxCursor !== null && !isDecimalCursor(serverMaxCursor)) ||
    !isDecimalCursor(localCursor)
  ) {
    clearEventCursor();
    notifyCursorReset();
    return null;
  }
  if (serverMaxCursor !== null && BigInt(localCursor) > BigInt(serverMaxCursor)) {
    clearEventCursor();
    notifyCursorReset();
    return null;
  }
  if (
    serverMinCursor !== null &&
    BigInt(serverMinCursor) > 0n &&
    BigInt(localCursor) + 1n < BigInt(serverMinCursor)
  ) {
    const replacement = (BigInt(serverMinCursor) - 1n).toString();
    setLastEventCursor(replacement);
    notifyCursorReset();
    return replacement;
  }
  return localCursor;
}

function isDecimalCursor(value: string): boolean {
  return /^\d+$/.test(value);
}

function notifyCursorReset(): void {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new Event(EVENT_CURSOR_RESET));
  }
}
