import Loader2 from "lucide-solid/icons/loader-2";
import { createSignal, For, Show } from "solid-js";
import type { AskAnswer } from "../../lib/api";
import { MarkdownOutput } from "../../components/MarkdownOutput";
import { AskCard, JobCard, ModelCard, PlanCard, ServiceCard } from "../session/SessionCards";
import type { SessionTimelineItem, ToolView } from "../session/sessionTimeline";
import { formatThoughtDuration } from "../session/sessionTimeline";
import { ThoughtRow } from "./ThoughtRow";

/**
 * Unified timeline item renderer - simpler than the old EventRow approach.
 * Mirrors bun version's ConversationItemRow architecture.
 */
export function TimelineItemRow(props: {
  item: SessionTimelineItem;
  onAnswer?: (askId: string, answer: AskAnswer) => Promise<void>;
}) {
  switch (props.item.type) {
    case "user":
      return <UserMessage item={props.item} />;
    case "assistant":
      return <AssistantMessage item={props.item} />;
    case "steer":
      return <SteerMessage item={props.item} />;
    case "tool":
      return <ToolMessage item={props.item} />;
    case "plan":
      return <PlanCard item={props.item} />;
    case "ask":
      return (
        <AskCard item={props.item} {...(props.onAnswer ? { onAnswer: props.onAnswer } : {})} />
      );
    case "model":
      return <ModelCard item={props.item} />;
    case "job":
      return <JobCard item={props.item} />;
    case "service":
      return <ServiceCard item={props.item} />;
    case "unknown":
      return (
        <article class="session-message session-message--tool">
          <header>{props.item.sourceKind}</header>
          <pre>{JSON.stringify(props.item.raw, null, 2)}</pre>
        </article>
      );
  }
}

function UserMessage(props: { item: Extract<SessionTimelineItem, { type: "user" }> }) {
  return (
    <div class="session-message session-message--user">
      <div class="session-message__user-content">
        <Show when={props.item.text}>
          <div class="session-message__bubble">{props.item.text}</div>
        </Show>
        <For each={props.item.attachments}>
          {(attachment) => (
            <span class="session-message__attachment" title={attachment.mime}>
              {attachment.name}
            </span>
          )}
        </For>
      </div>
    </div>
  );
}

function AssistantMessage(props: { item: Extract<SessionTimelineItem, { type: "assistant" }> }) {
  return (
    <>
      <Show when={props.item.reasoning}>
        {(reasoning) => (
          <ThoughtRow
            title={`Thought ${formatThoughtDuration(props.item.durationMs ?? undefined)}`.trim()}
            text={reasoning()}
            status="completed"
          />
        )}
      </Show>
      <div class="session-message session-message--assistant">
        <span class="session-message__dot" aria-hidden="true" />
        <div class="session-message__body">
          <MarkdownOutput text={props.item.text} />
        </div>
      </div>
    </>
  );
}

function SteerMessage(props: { item: Extract<SessionTimelineItem, { type: "steer" }> }) {
  return (
    <div class="session-message session-message--steer" role="note" aria-label="Steer">
      <span class="session-message__dot" aria-hidden="true" />
      <div class="session-message__body">
        <span class="muted">Steer: </span>
        {props.item.text}
      </div>
    </div>
  );
}

function ToolMessage(props: { item: Extract<SessionTimelineItem, { type: "tool" }> }) {
  const [open, setOpen] = createSignal(false);
  const view = props.item.view;

  // Check if this is a tool group (compressed)
  const isGroup = () => {
    const body = view.body;
    return (
      body.kind === "structured" &&
      body.value &&
      typeof body.value === "object" &&
      "tools" in body.value
    );
  };

  const dotTone = () => {
    switch (view.status) {
      case "success":
        return "success" as const;
      case "failure":
        return "danger" as const;
      case "running":
        return "muted" as const;
    }
  };

  const hasDetail = () => view.expandable && view.body.kind !== "none";

  // Simple non-expandable tool
  if (!hasDetail()) {
    return (
      <div class="tool-row tool-row--simple">
        <Show
          when={view.status === "running"}
          fallback={<span class="session-message__dot" data-tone={dotTone()} aria-hidden="true" />}
        >
          <Loader2 size={14} class="ui-spinner" />
        </Show>
        <span class="tool-row__title">{view.title}</span>
      </div>
    );
  }

  // Expandable tool or tool group
  return (
    <div class="collapsible-row">
      <button
        type="button"
        class={`collapsible-row__trigger ${open() ? "collapsible-row__trigger--open" : ""}`}
        onClick={() => setOpen(!open())}
      >
        <Show
          when={view.status === "running"}
          fallback={<span class="session-message__dot" data-tone={dotTone()} aria-hidden="true" />}
        >
          <Loader2 size={14} class="ui-spinner" />
        </Show>
        <span class="tool-row__title">{view.title}</span>
        <svg
          aria-hidden="true"
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          class={`collapsible-row__chevron ${open() ? "collapsible-row__chevron--open" : ""}`}
        >
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>
      <Show when={open()}>
        <div class="collapsible-row__detail">
          <ToolBodyContent view={view} isGroup={isGroup() === true} />
        </div>
      </Show>
    </div>
  );
}

function ToolBodyContent(props: { view: ToolView; isGroup: boolean }) {
  const body = props.view.body;

  // Handle tool group (compressed tools)
  if (props.isGroup && body.kind === "structured" && body.value) {
    const tools = (body.value as { tools: Array<{ id: string; title: string; status: string }> })
      .tools;
    return (
      <div class="tool-group__list">
        <For each={tools}>
          {(tool) => (
            <div class="tool-group__item">
              <span
                class="session-message__dot"
                data-tone={tool.status === "success" ? "success" : "danger"}
                aria-hidden="true"
              />
              <span class="tool-group__item-title">{tool.title}</span>
            </div>
          )}
        </For>
      </div>
    );
  }

  // Original tool body rendering
  switch (body.kind) {
    case "none":
      return null;
    case "patch":
      return <DiffBody patch={body.patch} />;
    case "text":
      return <pre class="session-event__terminal">{body.text}</pre>;
    case "structured":
      return <pre class="session-event__terminal">{JSON.stringify(body.value, null, 2)}</pre>;
    case "error":
      return (
        <div>
          <pre class="session-event__terminal session-event__terminal--err">{body.detail}</pre>
          <span class="session-event__exit">{body.code}</span>
        </div>
      );
    case "command_output":
      return (
        <div>
          <Show when={body.command}>
            <pre class="session-event__command">{body.command}</pre>
          </Show>
          <Show when={body.stdout}>
            <pre class="session-event__terminal">{body.stdout}</pre>
          </Show>
          <Show when={body.stderr}>
            <pre class="session-event__terminal session-event__terminal--err">{body.stderr}</pre>
          </Show>
          <Show when={body.truncated}>
            <span class="session-event__exit">Output truncated</span>
          </Show>
          <Show when={body.exitCode !== null}>
            <span class="session-event__exit">exit {body.exitCode}</span>
          </Show>
        </div>
      );
  }
}

function DiffBody(props: { patch: string }) {
  const lines = () => props.patch.split("\n");
  return (
    <pre class="session-event__diff">
      <For each={lines()}>
        {(line) => {
          const kind = line.startsWith("+")
            ? "add"
            : line.startsWith("-")
              ? "delete"
              : line.startsWith("@@")
                ? "hunk"
                : "context";
          return (
            <code class="session-event__diff-line" data-kind={kind}>
              {line}
              {"\n"}
            </code>
          );
        }}
      </For>
    </pre>
  );
}
