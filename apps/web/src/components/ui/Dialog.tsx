import X from "lucide-solid/icons/x";
import type { JSX } from "solid-js";
import { Portal } from "solid-js/web";

interface DialogProps {
  title: string;
  description: string;
  close: () => void;
  children: JSX.Element;
}

export function Dialog(props: DialogProps) {
  return (
    <Portal>
      <div class="ui-dialog-overlay">
        <section
          class="ui-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="ui-dialog-title"
          onKeyDown={(event) => {
            if (event.key === "Escape") props.close();
          }}
        >
          <header>
            <h2 id="ui-dialog-title">{props.title}</h2>
            <p>{props.description}</p>
          </header>
          <button type="button" class="ui-dialog-close" aria-label="Close" onClick={props.close}>
            <X size={16} />
          </button>
          {props.children}
        </section>
      </div>
    </Portal>
  );
}
