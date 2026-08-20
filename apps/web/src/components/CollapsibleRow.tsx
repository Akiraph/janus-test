import { ChevronRight } from "lucide-solid";
import { createUniqueId, type JSX, Show } from "solid-js";

interface CollapsibleRowProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  row: JSX.Element;
  detail: JSX.Element;
  detailClassName?: string;
}

/**
 * Simple collapsible row component, mirroring bun version's ConversationCollapsibleRow.
 * Used for tools, thoughts, and other expandable timeline items.
 */
export function CollapsibleRow(props: CollapsibleRowProps) {
  const detailId = createUniqueId();
  return (
    <div class="collapsible-row">
      <button
        type="button"
        class={`collapsible-row__trigger${props.open ? " collapsible-row__trigger--open" : ""}`}
        aria-expanded={props.open}
        aria-controls={detailId}
        onClick={() => props.onOpenChange(!props.open)}
      >
        {props.row}
        <ChevronRight
          size={13}
          aria-hidden="true"
          class={`collapsible-row__chevron${props.open ? " collapsible-row__chevron--open" : ""}`}
        />
      </button>
      <Show when={props.open}>
        <div
          id={detailId}
          class={`collapsible-row__detail${props.detailClassName ? ` ${props.detailClassName}` : ""}`}
        >
          {props.detail}
        </div>
      </Show>
    </div>
  );
}
