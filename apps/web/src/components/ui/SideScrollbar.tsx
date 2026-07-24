import { createEffect, createSignal, on, onCleanup, Show } from "solid-js";

/**
 * SideScrollbar
 *
 * A self-drawn, square, ultra-thin scrollbar overlay for a scroll container in
 * the IDE side panels. We use this instead of the native `::-webkit-scrollbar`
 * pseudo-element because Windows 11's "overlay scrollbar" system setting makes
 * Chromium ignore custom `::-webkit-scrollbar` rules and render the platform
 * (rounded) overlay scrollbar instead. A hand-painted thumb is impervious to
 * that: it reads the host's scrollTop/scrollHeight and positions a plain div,
 * so the square 3px thumb shows identically on every OS scrollbar setting.
 *
 * Usage: place the host scroll element with a ref, and render this component
 * either as a sibling of (or inside a relatively-positioned wrapper of) the
 * host. The wrapper's scroll surface stays `overflow:auto`; we only suppress
 * its native chrome via `.ide-sidebar-scroll--custom`.
 */
interface SideScrollbarProps {
  /** The scroll container to mirror. May start null until the ref resolves. */
  host: () => HTMLElement | null;
}

const THUMB_WIDTH = 3; // px — square, ultra-thin, matches the prior spec intent
const THUMB_MIN = 24; // px — never let it shrink to an unusable sliver
const HOVER_EXTEND = 10; // px — wider invisible hit area for grabbing

export function SideScrollbar(props: SideScrollbarProps) {
  const [visible, setVisible] = createSignal(false);
  const [thumbTop, setThumbTop] = createSignal(0);
  const [thumbH, setThumbH] = createSignal(0);
  const [hovering, setHovering] = createSignal(false);
  const [dragging, setDragging] = createSignal(false);

  let host: HTMLElement | null = null;
  let resize: ResizeObserver | null = null;
  let mut: MutationObserver | null = null;

  // Re-measure and reposition the thumb. Called on scroll, resize, and content
  // mutation of the host.
  function update() {
    const el = host;
    if (!el) return;
    const overflow = el.scrollHeight - el.clientHeight;
    if (overflow <= 1) {
      setVisible(false);
      return;
    }
    const ratio = el.clientHeight / el.scrollHeight;
    const h = Math.max(THUMB_MIN, Math.round(el.clientHeight * ratio));
    const track = el.clientHeight - h;
    const top = Math.round(track * (el.scrollTop / overflow));
    setThumbH(h);
    setThumbTop(top);
    setVisible(true);
  }

  function onScroll() {
    if (dragging()) return; // while dragging we drive scrollTop ourselves
    update();
  }

  function bindObservers(el: HTMLElement) {
    // Guard against double-bind: the Solid ref and the createEffect can both
    // resolve to the same element on first mount. If we already wired this exact
    // element, no-op; if it's a different element, tear the old observers down
    // first so we never leak listeners or duplicate observers.
    if (host === el) return;
    detach();
    host = el;
    el.addEventListener("scroll", onScroll, { passive: true });
    resize = new ResizeObserver(update);
    resize.observe(el);
    // Track descendants too — lazy tree loads change scrollHeight without a
    // scroll event or a host resize.
    mut = new MutationObserver(update);
    mut.observe(el, { childList: true, subtree: true, attributes: true });
    update();
  }

  function detach() {
    if (host) host.removeEventListener("scroll", onScroll);
    resize?.disconnect();
    mut?.disconnect();
    resize = null;
    mut = null;
  }

  // The Solid ref setter fires before this component's effects run; onCleanup
  // here would race. We react to props.host via an effect so binding happens at
  // a defined time and we can also handle a host that resolves late.
  createEffect(
    on(props.host, (el) => {
      if (el) bindObservers(el);
    }),
  );

  onCleanup(detach);

  // Drag the thumb to scroll. We translate pointer Y delta into scrollTop via
  // the track/overflow ratio so the thumb follows the cursor faithfully.
  let dragStartY = 0;
  let dragStartScroll = 0;
  function onPointerDown(e: PointerEvent) {
    const el = host;
    if (!el) return;
    e.preventDefault();
    dragStartY = e.clientY;
    dragStartScroll = el.scrollTop;
    setDragging(true);
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp, { once: true });
  }
  function onPointerMove(e: PointerEvent) {
    const el = host;
    if (!el) return;
    const overflow = el.scrollHeight - el.clientHeight;
    if (overflow <= 0) return;
    const track = el.clientHeight - thumbH();
    const delta = e.clientY - dragStartY;
    el.scrollTop = dragStartScroll + (delta / track) * overflow;
    update();
  }
  function onPointerUp() {
    setDragging(false);
    window.removeEventListener("pointermove", onPointerMove);
  }

  // The track is a transparent strip pinned to the right edge of the host,
  // wider than the visual thumb so it's easy to grab; the thumb is the 3px
  // square painted inside it.
  return (
    <Show when={visible()}>
      <div class="side-scroll-track" aria-hidden="true">
        <div
          class="side-scroll-thumb"
          classList={{ "side-scroll-thumb--active": dragging() || hovering() }}
          style={{ transform: `translateY(${thumbTop()}px)`, height: `${thumbH()}px` }}
          onPointerDown={onPointerDown}
          onMouseEnter={() => setHovering(true)}
          onMouseLeave={() => setHovering(false)}
        />
      </div>
    </Show>
  );
}
