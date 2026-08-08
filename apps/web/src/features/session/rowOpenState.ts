import { createSignal } from "solid-js";

/**
 * Persistent expand/collapse state keyed by timeline item id.
 *
 * Without this, every `<For>` re-render (which happens whenever the timeline
 * query is invalidated — i.e. on every `timeline.item_*` SSE event during a
 * running turn) re-mounts the row components, resetting their local
 * `createSignal(false)` open state and silently collapsing rows the user had
 * manually expanded (BUG 6). Keying the state here outlives component
 * remounts.
 *
 * Behavior:
 * - `isExplicit(id)` — true once the user has toggled this row. Auto-collapse
 *   logic only applies to rows the user has *not* touched, so a manual
 *   expand/collapse is never overwritten by the turn finishing.
 * - `isOpen(id, autoOpen)` — returns the effective open state. Uses the stored
 *   value when explicit, otherwise falls back to `autoOpen`.
 */

interface Entry {
  open: boolean;
  explicit: boolean;
}

const [entries, setEntries] = createSignal<ReadonlyMap<string, Entry>>(new Map());

export function rowOpenState(id: string, autoOpen: boolean): () => boolean {
  return () => {
    const entry = entries().get(id);
    return entry?.explicit ? entry.open : autoOpen;
  };
}

export function toggleRowOpen(id: string, effectiveOpen: boolean): void {
  setEntries((current) => {
    const next = new Map(current);
    next.set(id, {
      open: !effectiveOpen,
      explicit: true,
    });
    return next;
  });
}

/** Forget a row's explicit state so it follows autoOpen again. */
export function clearRowOpenState(id: string): void {
  setEntries((current) => {
    if (!current.has(id)) return current;
    const next = new Map(current);
    next.delete(id);
    return next;
  });
}
