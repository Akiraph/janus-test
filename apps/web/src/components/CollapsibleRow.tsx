import { ChevronRight } from "lucide-solid";
import { type JSX, Show } from "solid-js";

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
  return (
    <div class="collapsible-row">
      <button
        type="button"
        class={`collapsible-row__trigger${props.open ? " collapsible-row__trigger--open" : ""}`}
        onClick={() => props.onOpenChange(!props.open)}
      >
        {props.row}
        <ChevronRight
          size={13}
          class={`collapsible-row__chevron${props.open ? " collapsible-row__chevron--open" : ""}`}
        />
      </button>
      <Show when={props.open}>
        <div
          class={`collapsible-row__detail${props.detailClassName ? ` ${props.detailClassName}` : ""}`}
        >
          {props.detail}
        </div>
      </Show>
    </div>
  );
}
