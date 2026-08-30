import ChevronDown from "lucide-solid/icons/chevron-down";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import "./fields.css";

export interface SelectOption {
  readonly value: string;
  readonly label: string;
}

interface SelectProps {
  value: string;
  options: readonly SelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  "aria-label"?: string;
  class?: string;
}

const OPEN_KEYS = new Set(["Enter", " ", "ArrowDown", "ArrowUp"]);

/**
 * Lightweight custom dropdown. Native <select> styling varies across platforms
 * and cannot match the ui-input look, so we render a trigger button + a portal
 * list positioned beneath it. The list closes on outside pointerdown.
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
  let openedByKeyboard = false;

  const selectedLabel = () =>
    props.options.find((option) => option.value === props.value)?.label ?? props.value;

  const optionButtons = (): HTMLButtonElement[] =>
    listRef ? Array.from(listRef.querySelectorAll<HTMLButtonElement>(".ui-select-option")) : [];

  const focusOptionAt = (index: number) => {
    const buttons = optionButtons();
    if (buttons.length === 0) return;
    buttons[((index % buttons.length) + buttons.length) % buttons.length]?.focus();
  };

  const activeIndex = () => {
    const buttons = optionButtons();
    const active = document.activeElement;
    return active instanceof HTMLButtonElement ? buttons.indexOf(active) : -1;
  };

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

  const openList = (viaKeyboard = false) => {
    if (props.disabled) return;
    openedByKeyboard = viaKeyboard;
    position();
    setOpen(true);
  };

  const closeList = () => {
    // Never drop focus on the floor when the list unmounts under it.
    if (listRef?.contains(document.activeElement)) triggerRef?.focus();
    setOpen(false);
  };

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

  const onListKeyDown = (event: KeyboardEvent) => {
    const count = optionButtons().length;
    if (count === 0) return;
    const current = activeIndex();
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusOptionAt(current < 0 ? 0 : current + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        focusOptionAt(current < 0 ? count - 1 : current - 1);
        break;
      case "Home":
        event.preventDefault();
        focusOptionAt(0);
        break;
      case "End":
        event.preventDefault();
        focusOptionAt(count - 1);
        break;
      case "Tab":
        event.preventDefault();
        closeList();
        break;
    }
  };

  // Close on outside click or any scroll while open.
  createEffect(() => {
    if (!open()) return;
    // Re-measure now that the real list height is known and snap position.
    requestAnimationFrame(() => {
      refine();
      if (!openedByKeyboard) return;
      const selected = props.options.findIndex((option) => option.value === props.value);
      focusOptionAt(selected < 0 ? 0 : selected);
    });
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef?.contains(target)) return;
      if (listRef?.contains(target)) return;
      closeList();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      // Escape dismisses only the menu — an enclosing Dialog must stay open.
      event.stopPropagation();
      closeList();
      triggerRef?.focus();
    };
    const onScroll = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && listRef?.contains(target)) return;
      position();
      requestAnimationFrame(refine);
    };
    const onResize = () => closeList();
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
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
        disabled={props.disabled}
        onPointerDown={(event) => {
          // Primary button only; ignore right-click / pen barrel.
          if (event.button !== 0) return;
          // Prevent the subsequent click from re-toggling after we open here.
          event.preventDefault();
          toggle();
        }}
        onKeyDown={(event) => {
          if (!OPEN_KEYS.has(event.key)) return;
          event.preventDefault();
          if (!open()) openList(true);
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
              aria-label={props["aria-label"]}
              onKeyDown={onListKeyDown}
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
                    onKeyDown={(event) => {
                      if (event.key !== "Enter" && event.key !== " ") return;
                      event.preventDefault();
                      choose(option.value);
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
