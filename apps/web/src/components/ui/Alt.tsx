import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";

interface AltProps {
  /** Custom bubble content shown on hover/focus. */
  content: import("solid-js").JSX.Element;
  /** Horizontal offset from the cursor (right side). */
  offsetX?: number;
  /** Vertical offset from the cursor (upward is negative). */
  offsetY?: number;
  /** Hover delay in ms before showing the bubble. */
  delay?: number;
  /** Optional class on the bubble wrapper. */
  class?: string;
  /** The element that triggers the bubble on hover/focus. */
  children: import("solid-js").JSX.Element;
}

/**
 * Custom alt/tooltip that follows the pointer. Preferred placement is the
 * cursor's top-right corner. When near viewport edges it flips, using the
 * actual measured bubble size so bottom-of-list rows keep tracking the mouse
 * instead of snapping to a fixed clamp height.
 */
export function Alt(props: AltProps) {
  const [open, setOpen] = createSignal(false);
  const [coords, setCoords] = createSignal<{ top: number; left: number } | null>(null);
  let triggerRef: HTMLSpanElement | undefined;
  let bubbleRef: HTMLDivElement | undefined;
  const offsetX = () => props.offsetX ?? 10;
  const offsetY = () => props.offsetY ?? -10;
  const delay = () => props.delay ?? 40;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let pointer = { x: 0, y: 0 };
  let usingPointer = false;

  const placeAt = (x: number, y: number) => {
    const margin = 6;
    // Prefer measured size once the bubble is painted; fall back to estimates.
    const size = bubbleRef?.getBoundingClientRect();
    const width = size?.width && size.width > 0 ? size.width : 280;
    const height = size?.height && size.height > 0 ? size.height : 120;

    // Preferred: cursor top-right.
    let left = x + offsetX();
    let top = y + offsetY() - height;

    // If top-right would clip the right edge, flip to top-left.
    if (left + width > window.innerWidth - margin) {
      left = x - width - 6;
    }
    // If top-right would clip the top edge, flip below the cursor.
    if (top < margin) {
      top = y + 14;
    }
    // Final clamp — keep following the pointer, never pin to a fixed band.
    if (left < margin) left = margin;
    if (left + width > window.innerWidth - margin) {
      left = Math.max(margin, window.innerWidth - width - margin);
    }
    if (top < margin) top = margin;
    if (top + height > window.innerHeight - margin) {
      top = Math.max(margin, window.innerHeight - height - margin);
    }
    setCoords({ top, left });
  };

  const placeAtPointer = () => placeAt(pointer.x, pointer.y);

  const placeAtTrigger = () => {
    if (!triggerRef) return;
    const rect = triggerRef.getBoundingClientRect();
    placeAt(rect.left + 20, rect.top + 8);
  };

  const show = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      if (usingPointer) placeAtPointer();
      else placeAtTrigger();
      setOpen(true);
      // Re-measure after first paint so the real bubble height drives placement.
      requestAnimationFrame(() => {
        if (!open()) return;
        if (usingPointer) placeAtPointer();
        else placeAtTrigger();
      });
    }, delay());
  };

  const hide = () => {
    if (timer) clearTimeout(timer);
    usingPointer = false;
    setOpen(false);
  };

  const onPointerMove = (event: PointerEvent) => {
    usingPointer = true;
    pointer = { x: event.clientX, y: event.clientY };
    if (open()) placeAtPointer();
  };

  const onPointerEnter = (event: PointerEvent) => {
    usingPointer = true;
    pointer = { x: event.clientX, y: event.clientY };
    placeAtPointer();
    show();
  };

  const onScrollOrResize = () => {
    if (!open()) return;
    if (usingPointer) placeAtPointer();
    else placeAtTrigger();
  };

  onMount(() => {
    const el = triggerRef;
    if (!el) return;
    el.addEventListener("pointerenter", onPointerEnter);
    el.addEventListener("pointerleave", hide);
    el.addEventListener("pointermove", onPointerMove);
    el.addEventListener("focusin", show);
    el.addEventListener("focusout", hide);
  });

  createEffect(() => {
    if (!open()) return;
    window.addEventListener("scroll", onScrollOrResize, true);
    window.addEventListener("resize", onScrollOrResize);
    onCleanup(() => {
      window.removeEventListener("scroll", onScrollOrResize, true);
      window.removeEventListener("resize", onScrollOrResize);
    });
  });

  onCleanup(() => {
    if (timer) clearTimeout(timer);
  });

  return (
    <span class="alt-trigger" ref={triggerRef}>
      {props.children}
      <Show when={open() && coords()} keyed>
        {(c) => (
          <Portal>
            <div
              ref={bubbleRef}
              class={`alt-bubble ${props.class ?? ""}`}
              style={{ position: "fixed", top: `${c.top}px`, left: `${c.left}px` }}
              role="tooltip"
            >
              {props.content}
            </div>
          </Portal>
        )}
      </Show>
    </span>
  );
}
