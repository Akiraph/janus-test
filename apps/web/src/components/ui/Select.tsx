import { ChevronDown } from "lucide-solid";
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
    setCoords({ top: rect.bottom + 2, left: rect.left, width: rect.width });
  };

  const toggle = () => {
    if (open()) {
      setOpen(false);
      return;
    }
    position();
    setOpen(true);
  };

  const choose = (value: string) => {
    props.onChange(value);
    setOpen(false);
  };

  // Close on outside click or any scroll while open.
  createEffect(() => {
    if (!open()) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef?.contains(target)) return;
      if (listRef?.contains(target)) return;
      setOpen(false);
    };
    const onScroll = () => setOpen(false);
    const onResize = () => setOpen(false);
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointerDown, true);
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
        onClick={toggle}
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
                    onClick={() => choose(option.value)}
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
