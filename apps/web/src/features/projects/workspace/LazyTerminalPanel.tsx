import Loader2 from "lucide-solid/icons/loader-2";
import { type Component, lazy, Show, Suspense } from "solid-js";

/**
 * Dynamically load the Terminal panel only when the Project Terminal activity is
 * opened. Keeps @xterm out of the initial bundle.
 */
const LazyTerminalInner = lazy(() =>
  import("./TerminalPanel").then((m) => ({ default: m.TerminalPanel })),
);

export const LazyTerminalPanel: Component<Parameters<typeof LazyTerminalInner>[0]> = (props) => {
  return (
    <Suspense
      fallback={
        <div class="terminal-panel__loading" role="status" aria-label="Loading terminal">
          <Loader2 size={16} class="ui-spinner" />
          <span>Loading terminal…</span>
        </div>
      }
    >
      <Show when={props.active?.() ?? true}>
        <LazyTerminalInner {...props} />
      </Show>
    </Suspense>
  );
};
