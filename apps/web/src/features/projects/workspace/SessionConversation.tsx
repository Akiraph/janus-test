import ChevronRight from "lucide-solid/icons/chevron-right";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import { createEffect, createMemo, createSignal, For, type JSX, onCleanup, Show } from "solid-js";
import { EmptyState } from "../../../components/ui/EmptyState";
import { NotificationEvent } from "../../../components/ui/notifications";
import type {
  AskAnswer,
  AttachmentView,
  ContextUsageView,
  ProviderView,
  PublicLimits,
  QueuedTurnItem,
  SessionModelPreference,
  TurnSummary,
} from "../../../lib/api";
import { retryState } from "../../../lib/modelRetryState";
import { modelStreamOutput } from "../../../lib/modelStream";
import { MarkdownOutput } from "./MarkdownOutput";
import { QueuedMessagesBar } from "./QueuedMessagesBar";
import { rowOpenState, toggleRowOpen } from "./rowOpenState";
import { AskCard, JobCard, ModelCard, PlanCard, ServiceCard } from "./SessionCards";
import { SessionComposer, type SessionMessageReceipt } from "./SessionComposer";
import { isNearLatest, keepLatestContentVisible } from "./sessionScrollPolicy";
import {
  formatThoughtDuration,
  type SessionTimelineItem,
  type ToolActivityDetail,
  type ToolView,
} from "./sessionTimeline";
import { isTurnRunning } from "./turnPresentation";

interface SessionConversationProps {
  items: readonly SessionTimelineItem[];
  loading: boolean;
  error: string | null;
  delivery: "send" | "queue";
  composerDisabled?: boolean;
  contextUsage: ContextUsageView | null;
  limits: PublicLimits | undefined;
  modelPreference: SessionModelPreference | null;
  providers: readonly ProviderView[];
  turn: TurnSummary | null;
  provisionalUserTurnId: string | null;
  provisionalUserText: string;
  provisionalText: string;
  provisionalReasoning: string;
  provisionalRoundId: string | null;
  sessionId: string;
  onRetry: () => void;
  onSubmit: (
    content: string,
    modelPreference: SessionModelPreference | null,
    attachmentIds: readonly string[],
  ) => Promise<SessionMessageReceipt>;
  onUploadAttachment: (sessionId: string, file: File) => Promise<AttachmentView>;
  onDeleteAttachment: (sessionId: string, attachmentId: string) => Promise<void>;
  onAnswer?: (askId: string, answer: AskAnswer) => Promise<void>;
  onCancel?: (() => Promise<void>) | undefined;
  queuedTurns?: readonly QueuedTurnItem[] | undefined;
  onQueuedTurnCancel?: ((turn: QueuedTurnItem) => Promise<void>) | undefined;
}

export function SessionConversation(props: SessionConversationProps) {
  let scroller: HTMLDivElement | undefined;
  let activityObserver: ResizeObserver | undefined;
  let scrollFrame: number | undefined;
  let observedActivityContainer = false;
  let observedSessionId = "";
  let followLatest = true;

  function cancelScrollFrame() {
    if (scrollFrame === undefined) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = undefined;
  }

  function updateFollowLatest() {
    if (!scroller) return;
    followLatest = isNearLatest(scroller.scrollHeight, scroller.scrollTop, scroller.clientHeight);
  }

  function scheduleScrollToLatest() {
    cancelScrollFrame();
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = undefined;
      if (!scroller || !followLatest) return;
      keepLatestContentVisible(scroller, followLatest);
    });
  }

  function observeActivityContainer(element: HTMLDivElement) {
    activityObserver?.disconnect();
    if (typeof ResizeObserver !== "undefined") {
      activityObserver = new ResizeObserver(() => scheduleScrollToLatest());
      activityObserver.observe(element);
    }
    if (!observedActivityContainer) {
      observedActivityContainer = true;
      followLatest = true;
      scheduleScrollToLatest();
    }
  }

  function bottomTurn(): TurnStatusLike | null {
    if (props.turn) return props.turn;
    for (let index = props.items.length - 1; index >= 0; index -= 1) {
      const turn = props.items[index]?.turnStatus;
      if (turn) return turn as TurnStatusLike;
    }
    return null;
  }

  createEffect(() => {
    const sessionId = props.sessionId;
    if (sessionId === observedSessionId) return;
    observedSessionId = sessionId;
    followLatest = true;
    scheduleScrollToLatest();
  });

  onCleanup(() => {
    cancelScrollFrame();
    activityObserver?.disconnect();
    activityObserver = undefined;
  });

  return (
    <section class="session-conversation" aria-label="Conversation">
      <NotificationEvent
        message={props.error}
        variant="danger"
        action={{ label: "Retry", onClick: props.onRetry }}
      />
      <div
        class="session-conversation__timeline"
        ref={scroller}
        role="log"
        aria-live="polite"
        onScroll={updateFollowLatest}
      >
        <Show
          when={
            props.items.length > 0 ||
            Boolean(props.provisionalUserText) ||
            Boolean(props.provisionalText) ||
            Boolean(props.provisionalReasoning) ||
            props.turn !== null
          }
          fallback={
            <Show
              when={props.loading}
              fallback={
                <EmptyState
                  icon={MessageSquare}
                  title="Start a conversation"
                  description="Send a message to begin."
                />
              }
            >
              <div
                class="session-conversation__loading"
                role="status"
                aria-label="Loading conversation"
              >
                <Loader2 size={16} class="ui-spinner" />
              </div>
            </Show>
          }
        >
          <div
            class="session-conversation__items"
            ref={(element) => observeActivityContainer(element)}
          >
            <For each={props.items}>
              {(item) => (
                <ConversationEntry
                  item={item}
                  {...(props.onAnswer ? { onAnswer: props.onAnswer } : {})}
                />
              )}
            </For>
            <Show
              when={
                props.provisionalUserText &&
                !props.items.some(
                  (item) => item.type === "user" && item.turnId === props.provisionalUserTurnId,
                )
              }
            >
              <div class="session-message session-message--user">
                <div class="session-message__user-content">
                  <div class="session-message__bubble">{props.provisionalUserText}</div>
                </div>
              </div>
            </Show>
            <Show when={props.provisionalReasoning}>
              {(reasoning) => (
                <EventRow
                  itemId={`thinking:${props.provisionalRoundId ?? props.turn?.id ?? "live"}`}
                  title={props.provisionalText ? "Thought" : provisionalThinkingTitle()}
                  trailingChevron
                  tone="muted"
                  pulse
                  autoOpen={false}
                >
                  <div class="session-event__body-markdown">
                    <MarkdownOutput text={reasoning()} />
                  </div>
                </EventRow>
              )}
            </Show>
            <Show when={props.provisionalText}>
              {(text) => <AssistantOutput text={text()} provisional />}
            </Show>
            <Show when={bottomTurn()}>
              {(turn) => <TurnStatusOutput turn={turn()} sessionId={props.sessionId} />}
            </Show>
          </div>
        </Show>
      </div>

      <Show when={(props.queuedTurns ?? []).length > 0}>
        <QueuedMessagesBar
          turns={props.queuedTurns ?? []}
          onDelete={async (turn) => {
            if (!props.onQueuedTurnCancel) throw new Error("Queued cancellation is unavailable");
            await props.onQueuedTurnCancel(turn);
          }}
        />
      </Show>

      <SessionComposer
        delivery={props.delivery}
        disabled={props.composerDisabled ?? false}
        isRunning={Boolean(props.onCancel) && isTurnRunning(props.turn)}
        contextUsage={props.contextUsage}
        limits={props.limits}
        modelPreference={props.modelPreference}
        providers={props.providers}
        sessionId={props.sessionId}
        onSubmit={props.onSubmit}
        onUploadAttachment={props.onUploadAttachment}
        onDeleteAttachment={props.onDeleteAttachment}
        onCancel={props.onCancel}
      />
    </section>
  );
}

type TurnStatusLike = Pick<
  TurnSummary,
  "id" | "status" | "created_at" | "updated_at" | "completion_reason" | "cancellation_reason"
> & { model_attempt?: TurnSummary["model_attempt"] };

function TurnStatusOutput(props: { turn: TurnStatusLike | null; sessionId: string }) {
  // Tick once per second while a turn is active so the elapsed time display
  // stays fresh without waiting for a query refetch.
  const [tick, setTick] = createSignal(0);
  const visual = createMemo(() => {
    tick();
    return turnStatusVisual(props.turn, props.sessionId);
  });
  createEffect(() => {
    const turn = props.turn;
    if (!turn) return;
    const active = [
      "queued",
      "running",
      "waiting_for_job",
      "waiting_for_ask",
      "waiting_for_model",
      "canceling",
    ].includes(turn.status);
    if (!active) return;
    const id = setInterval(() => setTick((n) => n + 1), 1000);
    onCleanup(() => clearInterval(id));
  });
  return (
    <Show when={visual()}>
      {(status) => (
        <div
          class="session-message session-message--status"
          role={status().tone === "danger" ? "alert" : "status"}
        >
          <span
            class="session-message__dot"
            data-tone={status().tone}
            data-pulse={status().pulse ? "true" : undefined}
            aria-hidden="true"
          />
          <div class="session-message__body" data-tone={status().tone}>
            {status().text}
          </div>
        </div>
      )}
    </Show>
  );
}

interface TurnStatusVisual {
  text: string;
  tone: "muted" | "normal" | "warning" | "danger" | "success";
  pulse: boolean;
}

/**
 * The single persistent status row that lives in the conversation stream. It
 * renders turn progress as a dot + label and is the *only* place failure
 * reasons surface (BUG 3 + BUG 4):
 * - live model retries → `Reconnecting (X/5): reason` (dot pulses), fed by
 *   `model.attempt_retrying` SSE events via `retryState`, falling back to the
 *   durable `turn.model_attempt` projection captured by polling.
 * - terminal failure → `Failed: reason`, reading `completion_reason`.
 * - terminal success -> keep a compact `Worked for Xs` row in the timeline.
 */
function turnStatusVisual(turn: TurnStatusLike | null, sessionId: string): TurnStatusVisual | null {
  if (!turn) return null;
  const live = retryState(sessionId, turn.id);
  const retry =
    live ??
    (turn.model_attempt && turn.model_attempt.attempt > 0
      ? {
          attemptId: "",
          attempt: turn.model_attempt.attempt,
          maxAttempts: 5,
          detail: turn.model_attempt.detail ?? "model unavailable",
          retryAt: Date.now(),
        }
      : null);

  switch (turn.status) {
    case "queued":
      return { text: "Queued...", tone: "muted", pulse: true };
    case "running": {
      if (retry) {
        const retryInSeconds = Math.max(0, Math.ceil((retry.retryAt - Date.now()) / 1000));
        return {
          text: `Reconnecting (${retry.attempt}/${retry.maxAttempts} \u00b7 retrying in ${retryInSeconds}s): ${retry.detail}`,
          tone: "warning",
          pulse: true,
        };
      }
      const elapsed = formatElapsed(Date.now() - Date.parse(turn.created_at));
      const output = modelStreamOutput(sessionId, turn.id);
      const tokens = formatTokens(output);
      const thinking = formatThinkingDuration(output);
      const parts = [`Working (${elapsed}`];
      if (tokens) parts.push(` · ${tokens}`);
      if (thinking) parts.push(` · ${thinking}`);
      parts.push(")");
      return { text: parts.join(""), tone: "muted", pulse: true };
    }
    case "waiting_for_job":
      return { text: "Waiting for a job to finish...", tone: "warning", pulse: true };
    case "waiting_for_ask":
      return { text: "Waiting for your answer...", tone: "normal", pulse: true };
    case "waiting_for_model":
      return {
        text: turn.completion_reason || "Waiting for the model...",
        tone: "warning",
        pulse: true,
      };
    case "canceling":
      return { text: "Canceling...", tone: "warning", pulse: true };
    case "failed":
      return {
        text: turn.completion_reason || "Turn failed.",
        tone: "danger",
        pulse: false,
      };
    case "canceled":
      return { text: turn.cancellation_reason || "Canceled.", tone: "muted", pulse: false };
    case "completed":
    case "handed_off":
      return {
        text: `Worked for ${formatElapsed(Date.parse(turn.updated_at) - Date.parse(turn.created_at))}`,
        tone: "success",
        pulse: false,
      };
    default:
      return null;
  }
}

/** Format elapsed milliseconds as "Xs", "Xm Xs", or "Xh Xm". */
function formatElapsed(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "0s";
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}

/** Format usage into a compact token display string like "↓ 4.2k · ↑ 1.3k". */
function formatTokens(output: import("../../../lib/modelStream").ModelStreamOutput | null): string {
  if (!output?.usage) return "";
  const parts: string[] = [];
  if (output.usage.inputTokens > 0) parts.push(`↑ ${formatTokenCount(output.usage.inputTokens)}`);
  if (output.usage.outputTokens > 0) parts.push(`↓ ${formatTokenCount(output.usage.outputTokens)}`);
  return parts.join(" · ");
}

/** Format a token count: <1000 as-is, >=1000 as "X.Xk". */
function formatTokenCount(n: number): string {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(1)}k`;
}

/** Format thinking duration for the working status line. */
function formatThinkingDuration(
  output: import("../../../lib/modelStream").ModelStreamOutput | null,
): string {
  if (!output?.reasoningFirstSeenAt) return "";
  const endedAt = output.textFirstSeenAt ?? Date.now();
  const elapsed = Math.max(0, endedAt - output.reasoningFirstSeenAt);
  if (elapsed < 5000) return "";
  return `thinking for ${formatElapsed(elapsed)}`;
}

function provisionalThinkingTitle(): string {
  // Streaming reasoning changes to "Thought" as soon as answer text arrives;
  // the durable row adds the measured reasoning duration after the turn settles.
  return "Thinking...";
}

function ConversationEntry(props: {
  item: SessionTimelineItem;
  onAnswer?: (askId: string, answer: AskAnswer) => Promise<void>;
}) {
  switch (props.item.type) {
    case "user":
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
    case "assistant": {
      const item = props.item;
      return (
        <>
          <Show when={item.reasoning}>
            {(reasoning) => {
              const tail = formatThoughtDuration(item.durationMs);
              return (
                <EventRow
                  itemId={`thinking:${item.roundId ?? (item.turnId ? `${item.turnId}:${item.id}` : item.id)}`}
                  title={tail ? `Thought ${tail}` : "Thought"}
                  trailingChevron
                  tone="muted"
                  autoOpen={false}
                >
                  <div class="session-event__body-markdown">
                    <MarkdownOutput text={reasoning()} />
                  </div>
                </EventRow>
              );
            }}
          </Show>
          <Show when={item.text}>
            <AssistantOutput text={item.text} />
          </Show>
        </>
      );
    }
    case "steer":
      return (
        <div class="session-message session-message--steer" role="note" aria-label="Steer">
          <span class="session-message__dot" aria-hidden="true" />
          <div class="session-message__body">
            <span class="muted">Steer: </span>
            {props.item.text}
          </div>
        </div>
      );
    case "tool":
      return (
        <EventRow
          itemId={`tool:${props.item.id}`}
          title={props.item.view.title}
          tone={toolDotTone(props.item.view)}
          pulse={props.item.view.status === "running"}
          autoOpen={false}
          expandable={props.item.view.expandable}
          lowNoise={props.item.view.lowNoise}
          ariaLabel={props.item.view.title}
        >
          <ToolBody view={props.item.view} />
        </EventRow>
      );
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

function toolDotTone(view: ToolView): "muted" | "warning" | "danger" | "success" {
  switch (view.status) {
    case "success":
      return "success";
    case "failure":
      return "danger";
    case "running":
      return "muted";
  }
}

/** A timeline row that collapses to a one-line summary and expands to reveal a
 * body. One renderer serves both tool calls and thinking rows (BUG 1+2+6):
 * they share a status dot, a verb-style title, and expand/collapse affordance.
 * The collapse state is keyed by `itemId` in `rowOpenState`, so it survives
 * `<For>` re-mounts when the timeline query is invalidated mid-turn. The
 * chevron sits on the right for thinking rows (`trailingChevron`) to match the
 * requested `Thought >` alignment and on the left for tool rows. */
function EventRow(props: {
  itemId: string;
  title: string;
  tone: "muted" | "warning" | "danger" | "success";
  pulse?: boolean;
  expandable?: boolean;
  autoOpen: boolean;
  trailingChevron?: boolean;
  lowNoise?: boolean;
  ariaLabel?: string;
  children?: JSX.Element;
}) {
  const expandable = () => (props.expandable ?? true) && props.children != null;
  const open = rowOpenState(props.itemId, props.autoOpen);
  const onToggle = () => {
    if (expandable()) toggleRowOpen(props.itemId, open());
  };
  return (
    <article
      class={`session-event${props.lowNoise ? " session-event--low-noise" : ""}${
        props.trailingChevron ? " session-event--trailing" : ""
      }`}
      aria-label={props.ariaLabel ?? props.title}
    >
      <button
        type="button"
        class="session-event__summary"
        aria-expanded={expandable() ? open() : undefined}
        onClick={onToggle}
        disabled={!expandable()}
      >
        <span
          class="session-event__dot"
          data-tone={props.tone}
          data-pulse={props.pulse ? "true" : undefined}
          aria-hidden="true"
        />
        <span class="session-event__title">{props.title}</span>
        <Show when={expandable()}>
          <ChevronRight
            size={12}
            class="session-event__chevron"
            classList={{ "session-event__chevron--open": open() }}
          />
        </Show>
      </button>
      <Show when={expandable()}>
        <div
          class="session-event__body-wrap"
          classList={{ "session-event__body-wrap--open": open() }}
          aria-hidden={!open()}
        >
          <div class="session-event__body-content">
            <div class="session-event__body">{props.children}</div>
          </div>
        </div>
      </Show>
    </article>
  );
}

function ToolBody(props: { view: ToolView }) {
  const body = props.view.body;
  switch (body.kind) {
    case "none":
      return null;
    case "patch":
      return <DiffBody patch={body.patch} />;
    case "text":
      return <pre class="session-event__terminal">{body.text}</pre>;
    case "structured":
      return <pre class="session-event__terminal">{JSON.stringify(body.value, null, 2)}</pre>;
    case "activity":
      return <ActivityBody items={body.items} />;
    case "error":
      return (
        <>
          <pre class="session-event__terminal session-event__terminal--err">{body.detail}</pre>
          <span class="session-event__exit">{body.code}</span>
        </>
      );
    case "command_output":
      return (
        <>
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
        </>
      );
  }
}

function ActivityBody(props: { items: readonly ToolActivityDetail[] }) {
  return (
    <div class="session-event__activity">
      <For each={props.items}>
        {(item) => {
          if (item.kind === "thought") {
            return (
              <div class="session-event__activity-thought">
                <div class="session-event__activity-heading">
                  <span class="session-event__dot" data-tone="muted" aria-hidden="true" />
                  <span class="session-event__title">{item.title}</span>
                </div>
                <div class="session-event__activity-thought-body">
                  <MarkdownOutput text={item.text} />
                </div>
              </div>
            );
          }
          return (
            <div class="session-event__activity-tool">
              <EventRow
                itemId={`activity:${item.id}`}
                title={item.view.title}
                tone={toolDotTone(item.view)}
                pulse={item.view.status === "running"}
                autoOpen={false}
                expandable={item.view.expandable}
                lowNoise={item.view.lowNoise}
                ariaLabel={item.view.title}
              >
                <ToolBody view={item.view} />
              </EventRow>
            </div>
          );
        }}
      </For>
    </div>
  );
}

function DiffBody(props: { patch: string }) {
  const lines = createMemo(() => props.patch.split("\n"));
  return (
    <pre class="session-event__diff">
      <For each={lines()}>
        {(raw) => {
          const kind =
            raw.startsWith("+++") || raw.startsWith("---")
              ? "context"
              : raw.startsWith("@@")
                ? "hunk"
                : raw.startsWith("+")
                  ? "add"
                  : raw.startsWith("-")
                    ? "delete"
                    : "context";
          return (
            <code class={`session-event__diff-line`} data-kind={kind}>
              {raw || " "}
            </code>
          );
        }}
      </For>
    </pre>
  );
}

function AssistantOutput(props: { text: string; provisional?: boolean }) {
  const [visibleText, setVisibleText] = createSignal(props.text);
  let frame: number | undefined;
  createEffect(() => {
    const next = props.text;
    if (next === visibleText()) return;
    if (frame !== undefined) cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      frame = undefined;
      setVisibleText(next);
    });
  });
  onCleanup(() => {
    if (frame !== undefined) cancelAnimationFrame(frame);
  });
  return (
    <div
      class="session-message session-message--assistant"
      data-provisional={props.provisional ? "true" : undefined}
    >
      <span
        class="session-message__dot"
        data-pulse={props.provisional ? "true" : undefined}
        aria-hidden="true"
      />
      <div class="session-message__body">
        <Show when={visibleText()}>{(text) => <MarkdownOutput text={text()} />}</Show>
      </div>
    </div>
  );
}
