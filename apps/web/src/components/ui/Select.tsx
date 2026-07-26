import ChevronDown from "lucide-solid/icons/chevron-down";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";

export interface SelectOption {
  readonly value: string;
  readonly label: string;
}

interface SelectProps {
  value: string;
  options: readonly SelectOption[];
  onChange: (value: string) => void;
  "aria-label"?: string;
  class?: string;
}

/**
 * Lightweight custom dropdown. Native <select> styling varies across platforms
 * and cannot match the ui-input look, so we render a trigger button + a portal
 * list positioned beneath it. The list closes on outside pointerdown or scroll.
 *
 * Open on pointerdown (not click) so the menu appears on press, not mouseup —
 * that single frame difference is what makes it feel "attached" to the hand.
 */
export function Select(props: SelectProps) {
  const [open, setOpen] = createSignal(false);
  const [coords, setCoords] = createSignal<{ top: number; left: number; width: number } | null>(
    null,
  );
  let triggerRef: HTMLButtonElement | undefined;
  let listRef: HTMLDivElement | undefined;

  const selectedLabel = () =>
    props.options.find((option) => option.value === props.value)?.label ?? props.value;

  const position = () => {
    if (!triggerRef) return;
    const rect = triggerRef.getBoundingClientRect();
    // Default: open beneath the trigger. But when the trigger sits near the
    // bottom of the viewport (composer selectors at the page foot), the list
    // would clip off-screen — flip to open above the trigger instead.
    const minListHeight = 160;
    const spaceBelow = window.innerHeight - rect.bottom;
    const spaceAbove = rect.top;
    const openAbove = spaceBelow < minListHeight && spaceAbove >= spaceBelow;
    const top = openAbove ? Math.max(8, rect.top - minListHeight - 2) : rect.bottom + 2;
    // Write coords synchronously before open so the first paint already has
    // a correct fixed position — no empty-frame flash, no post-open reflow.
    setCoords({ top, left: rect.left, width: rect.width });
  };

  // After the list renders, re-measure its real height and snap the menu to
  // the trigger edge so a long option list does not float with the 160px guess.
  const refine = () => {
    const trigger = triggerRef;
    const list = listRef;
    if (!trigger || !list) return;
    const rect = trigger.getBoundingClientRect();
    const listRect = list.getBoundingClientRect();
    const spaceBelow = window.innerHeight - rect.bottom;
    const openAbove = spaceBelow < listRect.height && rect.top >= spaceBelow;
    const top = openAbove ? Math.max(8, rect.top - listRect.height - 2) : rect.bottom + 2;
    setCoords({ top, left: rect.left, width: rect.width });
  };

  const openList = () => {
    position();
    setOpen(true);
  };

  const closeList = () => setOpen(false);

  const toggle = () => {
    if (open()) {
      closeList();
      return;
    }
    openList();
  };

  const choose = (value: string) => {
    props.onChange(value);
    closeList();
  };

  // Close on outside click or any scroll while open.
  createEffect(() => {
    if (!open()) return;
    // Re-measure now that the real list height is known and snap position.
    requestAnimationFrame(refine);
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef?.contains(target)) return;
      if (listRef?.contains(target)) return;
      closeList();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeList();
        triggerRef?.focus();
      }
    };
    const onScroll = () => closeList();
    const onResize = () => closeList();
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
    });
  });

  return (
    <div
      classList={{
        "ui-select": true,
        "ui-select--open": open(),
        [props.class ?? ""]: !!props.class,
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        class="ui-select-trigger"
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-label={props["aria-label"]}
        onPointerDown={(event) => {
          // Primary button only; ignore right-click / pen barrel.
          if (event.button !== 0) return;
          // Prevent the subsequent click from re-toggling after we open here.
          event.preventDefault();
          toggle();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " " || event.key === "ArrowDown") {
            event.preventDefault();
            if (!open()) openList();
          }
        }}
      >
        <span class="ui-select-value">{selectedLabel()}</span>
        <ChevronDown
          classList={{ "ui-select-chevron": true, "ui-select-chevron--open": open() }}
          size={14}
        />
      </button>
      <Show when={open() && coords()}>
        {(box) => (
          <Portal>
            <div
              ref={listRef}
              class="ui-select-list"
              role="listbox"
              style={{ top: `${box().top}px`, left: `${box().left}px`, width: `${box().width}px` }}
            >
              <For each={props.options}>
                {(option) => (
                  <button
                    type="button"
                    role="option"
                    aria-selected={option.value === props.value}
                    classList={{
                      "ui-select-option": true,
                      "ui-select-option--selected": option.value === props.value,
                    }}
                    onPointerDown={(event) => {
                      if (event.button !== 0) return;
                      // Choose on press so selection feels immediate; prevent
                      // the document outside-click handler from racing us.
                      event.preventDefault();
                      event.stopPropagation();
                      choose(option.value);
                    }}
                  >
                    {option.label}
                  </button>
                )}
              </For>
            </div>
          </Portal>
        )}
      </Show>
    </div>
  );
}
