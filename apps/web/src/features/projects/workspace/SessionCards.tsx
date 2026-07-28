import AlertTriangle from "lucide-solid/icons/alert-triangle";
import BookOpen from "lucide-solid/icons/book-open";
import Briefcase from "lucide-solid/icons/briefcase";
import CircleHelp from "lucide-solid/icons/circle-help";
import ListTree from "lucide-solid/icons/list-tree";
import Loader2 from "lucide-solid/icons/loader-2";
import Server from "lucide-solid/icons/server";
import { createMemo, createSignal, For, type JSX, Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { Button } from "../../../components/ui/Button";
import type { TimelineItemView } from "../../../lib/api";

type Projection = Record<string, unknown>;

function asRecord(value: unknown): Projection {
  return value && typeof value === "object" ? (value as Projection) : {};
}

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

/** Plan card — rendered from plan / update_plan tool_call summaries or plan items. */
export function PlanCard(props: { projection: unknown; kind?: string }) {
  const p = () => asRecord(props.projection);
  const summary = () => asRecord(p().summary);
  const plan = () => {
    const raw = p().plan ?? summary().plan ?? p();
    return asRecord(raw);
  };
  const title = () =>
    str(plan().title) || str(p().title) || str(summary().plan_version_id) || "Plan";
  const steps = () => {
    const raw = plan().steps ?? plan().items ?? p().steps;
    return Array.isArray(raw) ? raw : [];
  };
  const sequence = () => summary().sequence ?? p().sequence;

  return (
    <article class="session-card session-card--plan" aria-label="Plan">
      <header class="session-card__head">
        <ListTree size={14} />
        <strong>{title()}</strong>
        <Show when={sequence() !== undefined && sequence() !== null}>
          <Badge>v{String(sequence())}</Badge>
        </Show>
      </header>
      <Show when={steps().length > 0}>
        <ol class="session-card__list">
          <For each={steps()}>
            {(step) => {
              const item = asRecord(step);
              return <li>{str(item.text ?? item.title ?? item.label, JSON.stringify(step))}</li>;
            }}
          </For>
        </ol>
      </Show>
      <Show when={steps().length === 0}>
        <pre class="session-card__pre">{JSON.stringify(plan(), null, 2)}</pre>
      </Show>
    </article>
  );
}

/** Ask card — blocking / best-effort question surfaces. Answer is composed via the session composer. */
export function AskCard(props: {
  projection: unknown;
  onAnswer: ((text: string) => void) | undefined;
  disabled: boolean | undefined;
}) {
  const p = () => asRecord(props.projection);
  const summary = () => asRecord(p().summary);
  const prompt = () =>
    str(p().prompt) || str(summary().prompt) || str(p().text) || "Waiting for an answer";
  const mode = () => str(p().mode ?? summary().mode, "blocking");
  const status = () => str(p().status ?? summary().status, "open");
  const choices = () => {
    const raw = p().choices ?? summary().choices;
    return Array.isArray(raw) ? raw.map((c) => str(c, String(c))) : [];
  };
  const [draft, setDraft] = createSignal("");
  const open = () => status() === "open" || status() === "pending" || status() === "";

  return (
    <article class="session-card session-card--ask" aria-label="Ask">
      <header class="session-card__head">
        <CircleHelp size={14} />
        <strong>Ask</strong>
        <Badge variant={open() ? "warning" : "success"}>{status() || mode()}</Badge>
        <Badge>{mode()}</Badge>
      </header>
      <p class="session-card__body">{prompt()}</p>
      <Show when={choices().length > 0}>
        <div class="session-card__choices">
          <For each={choices()}>
            {(choice) => (
              <Button
                size="sm"
                variant="outline"
                disabled={props.disabled || !open() || !props.onAnswer}
                onClick={() => props.onAnswer?.(choice)}
              >
                {choice}
              </Button>
            )}
          </For>
        </div>
      </Show>
      <Show when={open() && props.onAnswer && choices().length === 0}>
        <form
          class="session-card__answer"
          onSubmit={(event) => {
            event.preventDefault();
            const text = draft().trim();
            if (!text || props.disabled) return;
            props.onAnswer?.(text);
            setDraft("");
          }}
        >
          <input
            class="session-card__answer-input"
            value={draft()}
            placeholder="Answer…"
            disabled={props.disabled}
            onInput={(event) => setDraft(event.currentTarget.value)}
            aria-label="Answer ask"
          />
          <Button
            type="submit"
            size="sm"
            variant="primary"
            disabled={props.disabled || !draft().trim()}
          >
            Answer
          </Button>
        </form>
      </Show>
      <Show when={!props.onAnswer && open()}>
        <p class="session-card__hint muted">
          Use the composer below to answer. Dedicated Ask HTTP is not public yet.
        </p>
      </Show>
    </article>
  );
}

/** Model attempt / warning card. */
export function ModelCard(props: { projection: unknown; kind?: string }) {
  const p = () => asRecord(props.projection);
  const summary = () => asRecord(p().summary);
  const status = () => str(p().status ?? summary().status ?? p().classification, "attempt");
  const model = () =>
    str(p().model_id ?? p().model ?? summary().model_id ?? summary().model, "model");
  const detail = () => str(p().detail ?? p().message ?? summary().detail ?? summary().error);
  const attempt = () => p().attempt_number ?? summary().attempt_number;
  const warning = () =>
    props.kind === "model_warning" ||
    status().includes("fail") ||
    status().includes("cooldown") ||
    Boolean(p().warning);

  return (
    <article
      class="session-card session-card--model"
      classList={{ "session-card--warning": warning() }}
      aria-label="Model attempt"
    >
      <header class="session-card__head">
        <Show when={warning()} fallback={<BookOpen size={14} />}>
          <AlertTriangle size={14} />
        </Show>
        <strong>{model()}</strong>
        <Badge variant={warning() ? "warning" : "neutral"}>{status()}</Badge>
        <Show when={attempt() !== undefined && attempt() !== null}>
          <Badge>try {String(attempt())}</Badge>
        </Show>
      </header>
      <Show when={detail()}>
        <p class="session-card__body">{detail()}</p>
      </Show>
    </article>
  );
}

/** Job card — status + command summary. Controls are observation-first without public Job HTTP. */
export function JobCard(props: { projection: unknown }) {
  const p = () => asRecord(props.projection);
  const summary = () => asRecord(p().summary);
  const status = () => str(p().status ?? summary().status, "unknown");
  const command = () => str(p().command_summary ?? summary().command_summary ?? p().command, "Job");
  const jobId = () => str(p().job_id ?? summary().job_id ?? p().id);
  const active = () => ["queued", "running", "starting"].includes(status().toLowerCase());

  return (
    <article class="session-card session-card--job" aria-label="Job">
      <header class="session-card__head">
        <Briefcase size={14} />
        <strong class="session-card__mono" title={jobId()}>
          {command()}
        </strong>
        <Badge variant={active() ? "warning" : status().includes("fail") ? "danger" : "success"}>
          {status()}
        </Badge>
      </header>
      <Show when={jobId()}>
        <p class="session-card__meta mono">{jobId()}</p>
      </Show>
      <p class="session-card__hint muted">
        Job logs and cancel/stdin controls require the public Job API; status is projected from the
        timeline.
      </p>
    </article>
  );
}

/** Service card — impact + status. */
export function ServiceCard(props: { projection: unknown }) {
  const p = () => asRecord(props.projection);
  const summary = () => asRecord(p().summary);
  const status = () => str(p().status ?? summary().status, "unknown");
  const command = () =>
    str(p().command_summary ?? summary().command_summary ?? p().command, "Service");
  const impact = () => str(p().impact ?? summary().impact, "unknown");
  const serviceId = () => str(p().service_id ?? summary().service_id ?? p().id);
  const active = () => ["starting", "running", "restarting"].includes(status().toLowerCase());

  return (
    <article class="session-card session-card--service" aria-label="Service">
      <header class="session-card__head">
        <Server size={14} />
        <strong class="session-card__mono" title={serviceId()}>
          {command()}
        </strong>
        <Badge variant={active() ? "warning" : "neutral"}>{status()}</Badge>
        <Badge>{impact()}</Badge>
      </header>
      <Show when={serviceId()}>
        <p class="session-card__meta mono">{serviceId()}</p>
      </Show>
      <p class="session-card__hint muted">
        Stop/restart and log ranges will attach when Service HTTP is public. Current state is
        timeline-projected.
      </p>
    </article>
  );
}

/** Context / Compact status strip — honest empty when no projection exists. */
export function ContextCompactPanel(props: {
  items: TimelineItemView[];
  open: boolean;
  onClose: () => void;
}) {
  const contextItems = createMemo(() =>
    props.items.filter(
      (item) =>
        item.kind === "context" ||
        item.kind === "compact" ||
        item.kind === "compact_summary" ||
        (item.kind === "tool_call" && str(asRecord(item.projection).tool_name).includes("compact")),
    ),
  );
  const latest = () => contextItems()[contextItems().length - 1];

  return (
    <Show when={props.open}>
      <aside class="session-context-panel" aria-label="Context and Compact">
        <header class="session-context-panel__head">
          <strong>Context</strong>
          <Button size="sm" variant="ghost" onClick={props.onClose} aria-label="Close context">
            Close
          </Button>
        </header>
        <Show
          when={latest()}
          fallback={
            <p class="muted session-context-panel__empty">
              No Compact summary yet. Context estimates and manual Compact scheduling appear here
              when the supervisor publishes them; there is no public Compact HTTP route yet.
            </p>
          }
        >
          {(item) => (
            <div class="session-context-panel__body">
              <Badge>{item().kind}</Badge>
              <pre class="session-card__pre">
                {JSON.stringify(item().projection ?? {}, null, 2)}
              </pre>
            </div>
          )}
        </Show>
      </aside>
    </Show>
  );
}

/** Detect specialized card kind from a timeline item. */
export function specializedCardKind(
  kind: string,
  projection: unknown,
): "plan" | "ask" | "model" | "job" | "service" | null {
  const p = asRecord(projection);
  const summary = asRecord(p.summary);
  const tool = str(p.tool_name ?? summary.tool_name).toLowerCase();

  if (kind === "plan" || kind === "plan_version" || tool === "update_plan" || tool === "plan") {
    return "plan";
  }
  if (kind === "ask" || tool === "ask_user" || tool === "ask") {
    return "ask";
  }
  if (
    kind === "model_attempt" ||
    kind === "model_warning" ||
    kind === "model" ||
    tool.startsWith("model.")
  ) {
    return "model";
  }
  if (kind === "job" || tool === "job" || summary.job_id || p.job_id) {
    return "job";
  }
  if (kind === "service" || tool === "service" || summary.service_id || p.service_id) {
    return "service";
  }
  return null;
}

export function renderSpecializedCard(options: {
  kind: string;
  projection: unknown;
  onAskAnswer?: (text: string) => void;
  askDisabled?: boolean;
}): JSX.Element | null {
  const specialized = specializedCardKind(options.kind, options.projection);
  switch (specialized) {
    case "plan":
      return <PlanCard projection={options.projection} kind={options.kind} />;
    case "ask":
      return (
        <AskCard
          projection={options.projection}
          onAnswer={options.onAskAnswer}
          disabled={options.askDisabled}
        />
      );
    case "model":
      return <ModelCard projection={options.projection} kind={options.kind} />;
    case "job":
      return <JobCard projection={options.projection} />;
    case "service":
      return <ServiceCard projection={options.projection} />;
    default:
      return null;
  }
}

/** Tiny spinner used while a specialized card is hydrating. */
export function CardLoading() {
  return (
    <div class="session-card session-card--loading" role="status" aria-label="Loading">
      <Loader2 size={14} class="sessions-panel__spin" />
    </div>
  );
}
