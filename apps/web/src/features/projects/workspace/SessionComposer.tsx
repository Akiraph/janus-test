import Loader2 from "lucide-solid/icons/loader-2";
import Send from "lucide-solid/icons/send";
import { createSignal, Show } from "solid-js";
import { Button } from "../../../components/ui/Button";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";

export interface SessionMessageReceipt {
  route: string;
}

interface SessionComposerProps {
  delivery: "send" | "queue";
  disabled?: boolean;
  onSubmit: (content: string) => Promise<SessionMessageReceipt>;
}

export function SessionComposer(props: SessionComposerProps) {
  const [draft, setDraft] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal("");
  const [receipt, setReceipt] = createSignal<SessionMessageReceipt | null>(null);
  let textarea: HTMLTextAreaElement | undefined;

  const canSubmit = () => !props.disabled && !submitting() && Boolean(draft().trim());
  const actionLabel = () => (props.delivery === "queue" ? "Queue message" : "Send message");

  function resize() {
    if (!textarea) return;
    textarea.style.height = "auto";
    textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
  }

  async function submit() {
    const content = draft().trim();
    if (!content || !canSubmit()) return;

    setSubmitting(true);
    setError("");
    setReceipt(null);
    try {
      const result = await props.onSubmit(content);
      setDraft("");
      setReceipt(result);
      if (textarea) textarea.style.height = "auto";
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Message was not accepted");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form
      class="session-composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <Show when={error()}>
        <ErrorBlock variant="inline" message={error()} class="session-composer__error" />
      </Show>
      <Show when={receipt()?.route === "queued"}>
        <p class="session-composer__status" role="status">
          Message queued
        </p>
      </Show>
      <textarea
        ref={(element) => {
          textarea = element;
        }}
        class="session-composer__input"
        rows={1}
        placeholder={props.delivery === "queue" ? "Queue a message..." : "Send a message..."}
        value={draft()}
        disabled={props.disabled || submitting()}
        onInput={(event) => {
          setDraft(event.currentTarget.value);
          setReceipt(null);
          resize();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            void submit();
          }
        }}
      />
      <div class="session-composer__bar">
        <span class="session-composer__delivery">
          {props.delivery === "queue" ? "Next turn" : "Current session"}
        </span>
        <Button
          type="submit"
          variant="primary"
          size="sm"
          iconOnly
          disabled={!canSubmit()}
          aria-label={submitting() ? `${actionLabel()} in progress` : actionLabel()}
        >
          <Show when={submitting()} fallback={<Send size={16} />}>
            <Loader2 size={16} class="ui-spinner" />
          </Show>
        </Button>
      </div>
    </form>
  );
}
