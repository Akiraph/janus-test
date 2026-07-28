import AlertTriangle from "lucide-solid/icons/alert-triangle";
import BookOpen from "lucide-solid/icons/book-open";
import Briefcase from "lucide-solid/icons/briefcase";
import CircleHelp from "lucide-solid/icons/circle-help";
import ListTree from "lucide-solid/icons/list-tree";
import Server from "lucide-solid/icons/server";
import { createSignal, For, Show } from "solid-js";
import { Badge, type BadgeVariant } from "../../../components/ui/Badge";
import { Button } from "../../../components/ui/Button";
import type { SessionTimelineItem } from "./sessionTimeline";

type PlanItem = Extract<SessionTimelineItem, { type: "plan" }>;
type AskItem = Extract<SessionTimelineItem, { type: "ask" }>;
type ModelItem = Extract<SessionTimelineItem, { type: "model" }>;
type JobItem = Extract<SessionTimelineItem, { type: "job" }>;
type ServiceItem = Extract<SessionTimelineItem, { type: "service" }>;

export function PlanCard(props: { item: PlanItem }) {
  return (
    <article class="session-card session-card--plan" aria-label="Plan">
      <header class="session-card__head">
        <ListTree size={14} />
        <strong>{props.item.title}</strong>
        <Show when={props.item.sequence}>{(sequence) => <Badge>v{sequence()}</Badge>}</Show>
      </header>
      <Show when={props.item.steps.length > 0} fallback={<p class="muted">No plan steps</p>}>
        <ol class="session-card__list">
          <For each={props.item.steps}>
            {(step) => (
              <li>
                <span>{step.text}</span>
                <Show when={step.status}>
                  {(status) => <Badge variant={statusVariant(status())}>{status()}</Badge>}
                </Show>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </article>
  );
}

export function AskCard(props: {
  item: AskItem;
  onAnswer?: (askId: string, answer: string) => Promise<void>;
}) {
  const [draft, setDraft] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal("");
  const open = () => ["", "open", "pending"].includes(props.item.status.toLowerCase());
  const canAnswer = () => Boolean(props.item.askId && props.onAnswer && open());

  async function answer(value: string) {
    const askId = props.item.askId;
    const text = value.trim();
    if (!askId || !text || !props.onAnswer || submitting()) return;
    setSubmitting(true);
    setError("");
    try {
      await props.onAnswer(askId, text);
      setDraft("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Answer was not accepted");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <article class="session-card session-card--ask" aria-label="Ask">
      <header class="session-card__head">
        <CircleHelp size={14} />
        <strong>Ask</strong>
        <Badge variant={statusVariant(props.item.status)}>{props.item.status}</Badge>
        <Badge>{props.item.mode}</Badge>
      </header>
      <p class="session-card__body">{props.item.prompt}</p>
      <Show when={props.item.choices.length > 0}>
        <div class="session-card__choices">
          <For each={props.item.choices}>
            {(choice) => (
              <Show
                when={canAnswer()}
                fallback={<span class="session-card__choice">{choice}</span>}
              >
                <Button
                  size="sm"
                  variant="outline"
                  disabled={submitting()}
                  onClick={() => void answer(choice)}
                >
                  {choice}
                </Button>
              </Show>
            )}
          </For>
        </div>
      </Show>
      <Show when={canAnswer()}>
        <form
          class="session-card__answer"
          onSubmit={(event) => {
            event.preventDefault();
            void answer(draft());
          }}
        >
          <input
            class="session-card__answer-input"
            value={draft()}
            placeholder="Answer..."
            disabled={submitting()}
            onInput={(event) => setDraft(event.currentTarget.value)}
            aria-label="Answer ask"
          />
          <Button
            type="submit"
            size="sm"
            variant="primary"
            disabled={submitting() || !draft().trim()}
          >
            Answer
          </Button>
        </form>
      </Show>
      <Show when={error()}>
        <p class="session-card__error" role="alert">
          {error()}
        </p>
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
        <Badge variant={props.item.warning ? "warning" : statusVariant(props.item.status)}>
          {props.item.status}
        </Badge>
        <Show when={props.item.attempt}>{(attempt) => <Badge>try {attempt()}</Badge>}</Show>
      </header>
      <Show when={props.item.detail}>
        <p class="session-card__body">{props.item.detail}</p>
      </Show>
    </article>
  );
}

export function JobCard(props: { item: JobItem }) {
  return (
    <article class="session-card session-card--job" aria-label="Job">
      <header class="session-card__head">
        <Briefcase size={14} />
        <strong class="session-card__mono" title={props.item.jobId ?? undefined}>
          {props.item.command}
        </strong>
        <Badge variant={statusVariant(props.item.status)}>{props.item.status}</Badge>
      </header>
      <Show when={props.item.jobId}>{(id) => <p class="session-card__meta mono">{id()}</p>}</Show>
    </article>
  );
}

export function ServiceCard(props: { item: ServiceItem }) {
  return (
    <article class="session-card session-card--service" aria-label="Service">
      <header class="session-card__head">
        <Server size={14} />
        <strong class="session-card__mono" title={props.item.serviceId ?? undefined}>
          {props.item.command}
        </strong>
        <Badge variant={statusVariant(props.item.status)}>{props.item.status}</Badge>
        <Badge>{props.item.impact}</Badge>
      </header>
      <Show when={props.item.serviceId}>
        {(id) => <p class="session-card__meta mono">{id()}</p>}
      </Show>
    </article>
  );
}

function statusVariant(status: string): BadgeVariant {
  const value = status.toLowerCase();
  if (["failed", "failure", "error", "canceled", "interrupted"].includes(value)) {
    return "danger";
  }
  if (["queued", "running", "starting", "pending", "open", "waiting"].includes(value)) {
    return "warning";
  }
  if (["completed", "succeeded", "success", "ready", "stopped"].includes(value)) {
    return "success";
  }
  return "neutral";
}
