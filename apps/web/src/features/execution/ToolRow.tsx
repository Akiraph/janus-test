import { ChevronRight, Loader2 } from "lucide-solid";
import { createSignal, type JSX, Show } from "solid-js";

type ToolStatus = "running" | "success" | "failure";

interface ToolRowProps {
  title: string;
  status: ToolStatus;
  meta?: string;
  detail?: JSX.Element;
  compressible?: boolean;
}

/**
 * Simple tool row component, mirroring bun version's ActionRow.
 * Displays a tool call with status dot, title, meta info, and optional expandable detail.
 */
export function ToolRow(props: ToolRowProps) {
  const [open, setOpen] = createSignal(false);

  const dotTone = () => {
    switch (props.status) {
      case "success":
        return "success";
      case "failure":
        return "danger";
      case "running":
        return "muted";
    }
  };

  const row = (
    <>
      <Show
        when={props.status === "running"}
        fallback={<span class="session-message__dot" data-tone={dotTone()} aria-hidden="true" />}
      >
        <Loader2 size={14} class="ui-spinner" />
      </Show>
      <span class="tool-row__title">{props.title}</span>
      <Show when={props.meta}>
        <span class="tool-row__meta">{props.meta}</span>
      </Show>
    </>
  );

  if (props.detail === undefined) {
    return <div class="tool-row tool-row--simple">{row}</div>;
  }

  return (
    <div class="collapsible-row">
      <button
        type="button"
        class={`collapsible-row__trigger${open() ? " collapsible-row__trigger--open" : ""}`}
        onClick={() => setOpen(!open())}
      >
        {row}
        <ChevronRight
          aria-hidden="true"
          size={13}
          class={`collapsible-row__chevron${open() ? " collapsible-row__chevron--open" : ""}`}
        />
      </button>
      <Show when={open()}>
        <div class="collapsible-row__detail tool-row__detail">{props.detail}</div>
      </Show>
    </div>
  );
}

interface ToolGroupRowProps {
  title: string;
  count: number;
  detail: JSX.Element;
}

/**
 * Compressed tool group row - displays multiple tools as one row.
 * Example: "Read 3 Files", "Ran 2 Commands", "Read 2 Files and Ran 1 Command"
 */
export function ToolGroupRow(props: ToolGroupRowProps) {
  const [open, setOpen] = createSignal(false);

  const row = (
    <>
      <span class="session-message__dot" data-tone="success" aria-hidden="true" />
      <span class="tool-row__title">{props.title}</span>
      <span class="tool-row__meta">{props.count} items</span>
    </>
  );

  return (
    <div class="collapsible-row">
      <button
        type="button"
        class={`collapsible-row__trigger${open() ? " collapsible-row__trigger--open" : ""}`}
        onClick={() => setOpen(!open())}
      >
        {row}
        <ChevronRight
          aria-hidden="true"
          size={13}
          class={`collapsible-row__chevron${open() ? " collapsible-row__chevron--open" : ""}`}
        />
      </button>
      <Show when={open()}>
        <div class="collapsible-row__detail tool-row__detail tool-group__detail">
          {props.detail}
        </div>
      </Show>
    </div>
  );
}
