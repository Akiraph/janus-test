import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import { createEffect, For, Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { EmptyState } from "../../../components/ui/EmptyState";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import type {
  AttachmentView,
  ContextUsageView,
  ProviderView,
  PublicLimits,
  SessionModelPreference,
} from "../../../lib/api";
import { AskCard, JobCard, ModelCard, PlanCard, ServiceCard } from "./SessionCards";
import { SessionComposer, type SessionMessageReceipt } from "./SessionComposer";
import type { SessionTimelineItem } from "./sessionTimeline";

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
  sessionId: string;
  onRetry: () => void;
  onSubmit: (
    content: string,
    modelPreference: SessionModelPreference | null,
    attachmentIds: readonly string[],
  ) => Promise<SessionMessageReceipt>;
  onUploadAttachment: (sessionId: string, file: File) => Promise<AttachmentView>;
  onDeleteAttachment: (sessionId: string, attachmentId: string) => Promise<void>;
  onAnswer?: (askId: string, answer: string) => Promise<void>;
}

export function SessionConversation(props: SessionConversationProps) {
  let scroller: HTMLDivElement | undefined;
  let followLatest = true;

  createEffect(() => {
    const count = props.items.length;
    if (count > 0 && followLatest && scroller) {
      requestAnimationFrame(() => {
        if (scroller && followLatest) scroller.scrollTop = scroller.scrollHeight;
      });
    }
  });

  return (
    <section class="session-conversation" aria-label="Conversation">
      <div
        class="session-conversation__timeline"
        ref={scroller}
        role="log"
        aria-live="polite"
        onScroll={() => {
          if (!scroller) return;
          followLatest = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 80;
        }}
      >
        <Show
          when={!props.error}
          fallback={
            <ErrorBlock message={props.error ?? "Conversation failed"} retry={props.onRetry} />
          }
        >
          <Show
            when={props.items.length > 0}
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
            <div class="session-conversation__items">
              <For each={props.items}>
                {(item) => (
                  <SessionTimelineEntry
                    item={item}
                    {...(props.onAnswer ? { onAnswer: props.onAnswer } : {})}
                  />
                )}
              </For>
            </div>
          </Show>
        </Show>
      </div>

      <SessionComposer
        delivery={props.delivery}
        disabled={props.composerDisabled ?? false}
        contextUsage={props.contextUsage}
        limits={props.limits}
        modelPreference={props.modelPreference}
        providers={props.providers}
        sessionId={props.sessionId}
        onSubmit={props.onSubmit}
        onUploadAttachment={props.onUploadAttachment}
        onDeleteAttachment={props.onDeleteAttachment}
      />
    </section>
  );
}

function SessionTimelineEntry(props: {
  item: SessionTimelineItem;
  onAnswer?: (askId: string, answer: string) => Promise<void>;
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
    case "assistant":
      return (
        <div class="session-message session-message--assistant">
          <span class="session-message__dot" aria-hidden="true" />
          <div class="session-message__body">{props.item.text}</div>
        </div>
      );
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
        <article class="session-message session-message--tool" aria-label={props.item.toolName}>
          <header>
            <code>{props.item.toolName}</code>
            <Badge>{props.item.toolStatus}</Badge>
          </header>
          <pre>{JSON.stringify(props.item.summary, null, 2)}</pre>
        </article>
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
