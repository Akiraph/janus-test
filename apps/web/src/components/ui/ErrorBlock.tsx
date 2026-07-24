import { AlertCircle, RefreshCw } from "lucide-solid";
import { Show } from "solid-js";

export type ErrorBlockVariant = "inline" | "alert";

interface ErrorBlockProps {
  variant?: ErrorBlockVariant;
  message: string;
  retry?: () => void;
  class?: string;
}

export function ErrorBlock(props: ErrorBlockProps) {
  const variant = () => props.variant ?? "alert";
  return (
    <div
      class="ui-error"
      classList={{
        "ui-error--inline": variant() === "inline",
        [props.class ?? ""]: !!props.class,
      }}
      role="alert"
    >
      <Show when={variant() === "alert"}>
        <AlertCircle size={18} />
      </Show>
      <span>{props.message}</span>
      <Show when={props.retry}>
        <button type="button" class="ui-text-button" onClick={props.retry}>
          <RefreshCw size={14} />
          Retry
        </button>
      </Show>
    </div>
  );
}
