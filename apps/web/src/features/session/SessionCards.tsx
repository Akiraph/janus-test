import AlertTriangle from "lucide-solid/icons/alert-triangle";
import BookOpen from "lucide-solid/icons/book-open";
import ListTree from "lucide-solid/icons/list-tree";
import { For, Show } from "solid-js";
import type { SessionTimelineItem } from "./sessionTimeline";

type PlanItem = Extract<SessionTimelineItem, { type: "plan" }>;
type ModelItem = Extract<SessionTimelineItem, { type: "model" }>;

export function PlanCard(props: { item: PlanItem }) {
  return (
    <article class="session-card session-card--plan" aria-label="Plan">
      <header class="session-card__head">
        <ListTree size={14} />
        <strong>{props.item.title}</strong>
      </header>
      <Show when={props.item.steps.length > 0} fallback={<p class="muted">No plan steps</p>}>
        <ol class="session-card__list">
          <For each={props.item.steps}>
            {(step) => (
              <li>
                <span>{step.text}</span>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </article>
  );
}

export function ModelCard(props: { item: ModelItem }) {
  return (
    <article
      class="session-card session-card--model"
      classList={{ "session-card--warning": props.item.warning }}
      aria-label="Model attempt"
    >
      <header class="session-card__head">
        <Show when={props.item.warning} fallback={<BookOpen size={14} />}>
          <AlertTriangle size={14} />
        </Show>
        <strong>{props.item.model}</strong>
      </header>
      <Show when={props.item.detail}>
        <p class="session-card__body">{props.item.detail}</p>
      </Show>
    </article>
  );
}
