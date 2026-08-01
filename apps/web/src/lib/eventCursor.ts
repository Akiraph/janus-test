/**
 * Global event cursor manager for SSE reconnection.
 *
 * Persists the last received cursor in localStorage so that a page refresh
 * resumes from the correct position instead of replaying the full event
 * history.  Falls back to in-memory state when localStorage is unavailable
 * (private browsing, storage errors).
 */

const STORAGE_KEY = "janus:event-cursor";

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
