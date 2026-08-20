import { Loader2 } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import { CollapsibleRow } from "../../components/CollapsibleRow";
import { MarkdownOutput } from "../../components/MarkdownOutput";

type ThoughtStatus = "running" | "completed";

interface ThoughtRowProps {
  /** Title text (e.g. "Thinking", "Thought for 6s"). */
  title: string;
  text: string;
  status: ThoughtStatus;
}

/**
 * Collapsible reasoning row. Defaults to collapsed both while streaming and
 * after completion; the user opens it to peek at the chain of thought.
 */
export function ThoughtRow(props: ThoughtRowProps) {
  const [open, setOpen] = createSignal(false);

  const row = (
    <>
      <Show
        when={props.status === "running"}
        fallback={<span class="session-message__dot" data-tone="muted" aria-hidden="true" />}
      >
        <Loader2 size={14} class="ui-spinner" aria-hidden="true" />
      </Show>
      <span class="thought-row__title">{props.title}</span>
    </>
  );

  const detail = (
    <div class="thought-row__text">
      <MarkdownOutput text={props.text} />
    </div>
  );

  return (
    <CollapsibleRow
      open={open()}
      onOpenChange={setOpen}
      row={row}
      detail={detail}
      detailClassName="thought-row__detail"
    />
  );
}
