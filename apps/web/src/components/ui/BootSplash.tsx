import { Show } from "solid-js";
import { JanusLogo } from "../JanusLogo";

interface BootSplashProps {
  variant?: "loading" | "error";
  message?: string;
}

/**
 * A lightweight centered splash used during the initial bootstrap round-trip
 * and as Suspense fallback while a lazy route's first query resolves — anything
 * beats a blank white page. Just the wordmark with a gentle breath so the app
 * clearly loaded while data arrives. `variant="error"` swaps the breath for a
 * short message (used when the server can't be reached).
 */
export function BootSplash(props: BootSplashProps) {
  return (
    <div class="app-bootstrap" role="status" aria-label="Loading Janus">
      <Show when={props.variant !== "error"} fallback={<JanusLogo size={32} />}>
        <JanusLogo size={32} class="app-bootstrap-logo" />
      </Show>
      <Show when={props.variant === "error"}>
        <p>{props.message ?? "Couldn’t reach the Janus server."}</p>
      </Show>
    </div>
  );
}
