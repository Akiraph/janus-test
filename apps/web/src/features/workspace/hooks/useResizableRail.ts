import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Right-rail resize + collapse state.
 *
 * Width is a UI-only concern (no server state): the user drags the rail edge to
 * resize between RAIL_MIN and RAIL_MAX, and the choice is remembered across
 * sessions via localStorage. Collapse is a separate boolean so a collapsed rail
 * restores to its last width when reopened.
 *
 * Dragging attaches pointer listeners to window so the drag keeps tracking even
 * if the pointer leaves the handle element. Width is clamped on every move so
 * the rail can never escape its bounds.
 */

export const RAIL_MIN_WIDTH = 280;
export const RAIL_MAX_WIDTH = 480;
export const RAIL_DEFAULT_WIDTH = 320;
const RAIL_WIDTH_KEY = "janus.session-rail.width";
const RAIL_COLLAPSED_KEY = "janus.session-rail.collapsed";

function readStoredWidth(): number {
  if (typeof window === "undefined") return RAIL_DEFAULT_WIDTH;
  const raw = window.localStorage.getItem(RAIL_WIDTH_KEY);
  const parsed = raw === null ? NaN : Number(raw);
  if (Number.isFinite(parsed)) {
    return Math.min(Math.max(parsed, RAIL_MIN_WIDTH), RAIL_MAX_WIDTH);
  }
  return RAIL_DEFAULT_WIDTH;
}

function readStoredCollapsed(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(RAIL_COLLAPSED_KEY) === "1";
}

export interface UseResizableRailResult {
  /** Current width in px (only meaningful when `collapsed` is false). */
  readonly width: number;
  readonly collapsed: boolean;
  /**
   * True while the user is actively drag-resizing. Lets callers drop the width
   * transition mid-drag so the handle tracks the pointer without easing lag.
   */
  readonly isDragging: boolean;
  /** Starts a resize drag — attach to the drag handle's onPointerDown. */
  readonly onResizeStart: (event: React.PointerEvent<HTMLDivElement>) => void;
  readonly expand: () => void;
  readonly toggleCollapsed: () => void;
}

export function useResizableRail(): UseResizableRailResult {
  const [width, setWidth] = useState<number>(readStoredWidth);
  const [collapsed, setCollapsed] = useState<boolean>(readStoredCollapsed);
  const [isDragging, setIsDragging] = useState(false);
  const draggingRef = useRef(false);
  // Rail's right edge (viewport x) captured at drag start. Width = rightEdge -
  // pointerX, so we can't assume the rail hugs the viewport right edge (the
  // workspace shell has padding/borders + a left sidebar, so the rail's right
  // edge is inset). Stashing the live rect avoids that offset.
  const railRightRef = useRef(0);

  // Persist width + collapse so the user's layout survives reloads.
  useEffect(() => {
    window.localStorage.setItem(RAIL_WIDTH_KEY, String(width));
  }, [width]);
  useEffect(() => {
    window.localStorage.setItem(RAIL_COLLAPSED_KEY, collapsed ? "1" : "0");
  }, [collapsed]);

  useEffect(() => {
    const onMove = (event: PointerEvent) => {
      if (!draggingRef.current) return;
      // Rail is on the right edge of the editor area, so width = rail right -
      // pointer x. Using the captured right edge keeps the handle glued to the
      // pointer even though the rail isn't flush with the viewport.
      const next = railRightRef.current - event.clientX;
      setWidth(Math.min(Math.max(next, RAIL_MIN_WIDTH), RAIL_MAX_WIDTH));
    };
    const onUp = () => {
      draggingRef.current = false;
      setIsDragging(false);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, []);

  const onResizeStart = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      event.preventDefault();
      // The handle sits on the rail's left edge; its parent is the rail
      // container. Read the rail's right edge from its bounding rect so the
      // width calc is correct regardless of where the rail sits in the page.
      const rail = event.currentTarget.parentElement;
      railRightRef.current =
        rail === null ? window.innerWidth : rail.getBoundingClientRect().right;
      draggingRef.current = true;
      setIsDragging(true);
      document.body.style.userSelect = "none";
      document.body.style.cursor = "col-resize";
    },
    [],
  );

  const toggleCollapsed = useCallback(() => setCollapsed((c) => !c), []);
  const expand = useCallback(() => setCollapsed(false), []);

  return {
    width,
    collapsed,
    isDragging,
    onResizeStart,
    expand,
    toggleCollapsed,
  };
}
