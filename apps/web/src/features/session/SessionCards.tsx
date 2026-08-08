import AlertTriangle from "lucide-solid/icons/alert-triangle";
import BookOpen from "lucide-solid/icons/book-open";
import Briefcase from "lucide-solid/icons/briefcase";
import CircleHelp from "lucide-solid/icons/circle-help";
import ListTree from "lucide-solid/icons/list-tree";
import Server from "lucide-solid/icons/server";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { type AskAnswer, getErrorMessage } from "../../lib/api";
import type { AskChoice, SessionTimelineItem } from "./sessionTimeline";

type PlanItem = Extract<SessionTimelineItem, { type: "plan" }>;
type AskItem = Extract<SessionTimelineItem, { type: "ask" }>;
type ModelItem = Extract<SessionTimelineItem, { type: "model" }>;
type JobItem = Extract<SessionTimelineItem, { type: "job" }>;
type ServiceItem = Extract<SessionTimelineItem, { type: "service" }>;
type AskResolution = { kind: "answered"; answer: AskAnswer } | { kind: "declined" };

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

export function AskCard(props: {
  item: AskItem;
  onAnswer?: (askId: string, answer: AskAnswer) => Promise<void>;
}) {
  const [draft, setDraft] = createSignal("");
  const [selectedChoices, setSelectedChoices] = createSignal<string[]>([]);
  const [customSelected, setCustomSelected] = createSignal(false);
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal("");
  const [resolution, setResolution] = createSignal<AskResolution | null>(null);
  const open = () => ["", "open", "pending"].includes(props.item.status.toLowerCase());
  const isNonBlocking = () => {
    const mode = props.item.mode.toLowerCase().replace(/[-_]/g, "");
    return mode === "besteffort" || mode === "nonblocking";
  };
  const expiresAtMillis = () => {
    if (!props.item.expiresAt) return null;
    const value = Date.parse(props.item.expiresAt);
    return Number.isFinite(value) ? value : null;
  };
  const [clock, setClock] = createSignal(Date.now());
  const remainingMillis = createMemo(() => {
    const expiresAt = expiresAtMillis();
    return expiresAt === null ? null : expiresAt - clock();
  });
  const remainingLabel = createMemo(() => {
    const remaining = remainingMillis();
    if (!isNonBlocking() || remaining === null || !open()) return "";
    return remaining <= 0 ? "Expired" : `Expires in ${formatRemaining(remaining)}`;
  });
  createEffect(() => {
    if (!isNonBlocking() || expiresAtMillis() === null || !open()) return;
    setClock(Date.now());
    const timer = window.setInterval(() => setClock(Date.now()), 1000);
    onCleanup(() => window.clearInterval(timer));
  });
  const askCanAnswer = () => {
    const remaining = remainingMillis();
    return Boolean(
      props.item.askId &&
        props.onAnswer &&
        open() &&
        (!isNonBlocking() || remaining === null || remaining > 0),
    );
  };
  const canAnswer = () => askCanAnswer() && resolution() === null;

  const answerValues = () => {
    const answer = props.item.answer;
    if (Array.isArray(answer)) return answer;
    return answer ? [answer] : [];
  };

  function choiceSelected(choice: AskChoice) {
    if (canAnswer()) return selectedChoices().includes(choice.label);
    return answerValues().includes(choice.label);
  }

  function customAnswerSelected() {
    if (canAnswer()) return customSelected();
    return (
      props.item.answer !== null &&
      !props.item.choices.some((choice) => answerValues().includes(choice.label))
    );
  }

  function selectChoice(choice: AskChoice, checked: boolean) {
    if (multiple()) {
      setSelectedChoices((current) =>
        checked
          ? current.includes(choice.label)
            ? current
            : [...current, choice.label]
          : current.filter((value) => value !== choice.label),
      );
    } else {
      setSelectedChoices(checked ? [choice.label] : []);
      setCustomSelected(false);
      setDraft("");
    }
  }

  function selectCustom(checked: boolean) {
    if (!multiple() && checked) setSelectedChoices([]);
    setCustomSelected(checked);
    if (!checked) setDraft("");
  }

  function answerValue(): AskAnswer | null {
    const custom = customSelected() ? draft().trim() : "";
    if (multiple()) {
      const values = [...selectedChoices()];
      if (custom) values.push(custom);
      return values.length > 0 ? values : null;
    }
    return custom || selectedChoices()[0] || null;
  }

  async function answer(value: AskAnswer) {
    const askId = props.item.askId;
    if (!askId || !props.onAnswer || submitting()) return;
    setSubmitting(true);
    setError("");
    try {
      await props.onAnswer(askId, value);
      setResolution(
        typeof value === "string" && value === "I decline to answer."
          ? { kind: "declined" }
          : { kind: "answered", answer: value },
      );
      setDraft("");
      setSelectedChoices([]);
      setCustomSelected(false);
    } catch (cause) {
      setError(getErrorMessage(cause, "Answer was not accepted"));
    } finally {
      setSubmitting(false);
    }
  }

  async function submit() {
    const value = answerValue();
    if (value !== null) await answer(value);
  }

  const multiple = () => props.item.multiple;
  const choiceName = () => `ask-${props.item.askId ?? props.item.id}`;
  const resolvedAnswer = (): AskAnswer | null => {
    const local = resolution();
    if (local?.kind === "answered") return local.answer;
    return props.item.answer;
  };
  const declined = () => {
    const local = resolution();
    return (
      local?.kind === "declined" ||
      (typeof props.item.answer === "string" && props.item.answer === "I decline to answer.")
    );
  };
  const answerText = (): string => {
    const answer = resolvedAnswer();
    if (typeof answer === "string") return answer;
    if (answer) return Array.from(answer).join(", ");
    return "No answer was provided.";
  };
  const hasResult = () =>
    resolution() !== null ||
    props.item.answer !== null ||
    ["answered", "expired"].includes(props.item.status.toLowerCase());

  return (
    <form
      class="session-card session-card--ask"
      aria-label="Ask"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <header class="session-card__head">
        <CircleHelp size={14} />
        <strong>Ask</strong>
        <Show when={remainingLabel()}>
          {(label) => <span class="session-card__ask-expiry">{label()}</span>}
        </Show>
      </header>
      <p class="session-card__body">{props.item.prompt}</p>
      <Show when={canAnswer()}>
        <fieldset class="session-card__choices" aria-label="Choices">
          <For each={props.item.choices}>
            {(choice, index) => (
              <div
                class="session-card__choice"
                classList={{ "session-card__choice--selected": choiceSelected(choice) }}
              >
                <input
                  type={multiple() ? "checkbox" : "radio"}
                  name={choiceName()}
                  aria-label={`${index() + 1}. ${choice.label}`}
                  checked={choiceSelected(choice)}
                  disabled={submitting()}
                  onChange={(event) => selectChoice(choice, event.currentTarget.checked)}
                />
                <span class="session-card__choice-content">
                  <span class="session-card__choice-label">
                    <span class="session-card__choice-number">{index() + 1}.</span>
                    {choice.label}
                  </span>
                  <Show when={choice.annotation}>
                    {(annotation) => (
                      <span class="session-card__choice-annotation">{annotation()}</span>
                    )}
                  </Show>
                </span>
              </div>
            )}
          </For>
          <div
            class="session-card__choice session-card__choice--custom"
            classList={{ "session-card__choice--selected": customAnswerSelected() }}
          >
            <input
              type={multiple() ? "checkbox" : "radio"}
              name={choiceName()}
              aria-label={`${props.item.choices.length + 1}. Enter an answer`}
              checked={customAnswerSelected()}
              disabled={submitting()}
              onChange={(event) => selectCustom(event.currentTarget.checked)}
            />
            <span class="session-card__choice-label session-card__choice-label--custom">
              <span class="session-card__choice-number">{props.item.choices.length + 1}.</span>
              Enter an answer
            </span>
            <Show when={customSelected()}>
              <input
                class="ui-input session-card__answer-input"
                value={draft()}
                placeholder="Answer..."
                disabled={submitting()}
                onInput={(event) => setDraft(event.currentTarget.value)}
                aria-label="Answer ask"
              />
            </Show>
          </div>
        </fieldset>
      </Show>
      <Show when={canAnswer()}>
        <div class="session-card__actions">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={submitting()}
            onClick={() => void answer("I decline to answer.")}
          >
            Decline
          </Button>
          <Button
            type="submit"
            size="sm"
            variant="primary"
            disabled={submitting() || answerValue() === null}
          >
            Submit
          </Button>
        </div>
      </Show>
      <Show when={!canAnswer() && hasResult()}>
        <p class="session-card__answer-result">
          {declined() ? "User declined to answer." : `User answered the question: ${answerText()}`}
        </p>
      </Show>
      <Show when={error()}>
        <p class="session-card__error" role="alert">
          {error()}
        </p>
      </Show>
    </form>
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

export function JobCard(props: { item: JobItem }) {
  return (
    <article class="session-card session-card--job" aria-label="Job">
      <header class="session-card__head">
        <Briefcase size={14} />
        <strong class="session-card__mono" title={props.item.jobId ?? undefined}>
          {props.item.command}
        </strong>
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
      </header>
      <Show when={props.item.serviceId}>
        {(id) => <p class="session-card__meta mono">{id()}</p>}
      </Show>
    </article>
  );
}

function formatRemaining(milliseconds: number): string {
  const totalSeconds = Math.max(1, Math.ceil(milliseconds / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}
