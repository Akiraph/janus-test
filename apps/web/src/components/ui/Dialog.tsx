import X from "lucide-solid/icons/x";
import { createUniqueId, type JSX, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import "./dialog.css";

interface DialogProps {
  title: string;
  description: string;
  close: () => void;
  children: JSX.Element;
}

const FOCUSABLE = "a[href], button, input, select, textarea, [tabindex]";

export function Dialog(props: DialogProps) {
  const titleId = createUniqueId();
  const descriptionId = createUniqueId();
  let panel!: HTMLElement;

  const focusables = () => {
    const all = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE));
    return all.filter((el) => el.tabIndex >= 0 && !el.matches(":disabled"));
  };

  onMount(() => {
    const opener = document.activeElement;
    const items = focusables();
    (items.find((el) => !el.classList.contains("ui-dialog-close")) ?? items[0])?.focus();
    onCleanup(() => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    });
  });

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      props.close();
      return;
    }
    if (event.key !== "Tab") return;
    const items = focusables();
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) return;
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <Portal>
      <div class="ui-dialog-overlay">
        <section
          ref={panel}
          class="ui-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby={titleId}
          aria-describedby={descriptionId}
          onKeyDown={onKeyDown}
        >
          <header>
            <h2 id={titleId}>{props.title}</h2>
            <p id={descriptionId}>{props.description}</p>
          </header>
          <button type="button" class="ui-dialog-close" aria-label="Close" onClick={props.close}>
            <X size={16} aria-hidden="true" />
          </button>
          {props.children}
        </section>
      </div>
    </Portal>
  );
}
