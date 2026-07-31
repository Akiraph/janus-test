import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { Badge, type BadgeVariant } from "../../../components/ui/Badge";
import {
  deleteSessionAttachment,
  postSessionMessage,
  type SessionModelPreference,
  uploadSessionAttachment,
} from "../../../lib/api";
import { clearRetryState } from "../../../lib/modelRetryState";
import {
  clearModelStreamText,
  isModelStreamOutputDurable,
  modelStreamOutput,
} from "../../../lib/modelStream";
import {
  useBootstrap,
  useProviders,
  useSession,
  useSessionContext,
  useSessionDiff,
  useSessionTimeline,
  useTurn,
} from "../../../lib/queries";
import { JobCard } from "./SessionCards";
import { SessionConversation } from "./SessionConversation";
import { SessionDiffView } from "./SessionDiffView";
import { decodeSessionDiff } from "./sessionDiff";
import { decodeSessionTimeline } from "./sessionTimeline";
import "./session.css";

export type SessionSubView = "main" | "diff" | "async";

interface SessionTabViewProps {
  sessionId: () => string;
  subView: () => SessionSubView;
  onSubViewChange: (view: SessionSubView) => void;
  onTitle?: (title: string) => void;
}

export function SessionTabView(props: SessionTabViewProps) {
  const session = useSession(props.sessionId);
  const bootstrap = useBootstrap();
  const timeline = useSessionTimeline(props.sessionId);
  const context = useSessionContext(props.sessionId);
  const providers = useProviders();
  const diff = useSessionDiff(props.sessionId, () => props.subView() === "diff");
  const [acceptedVersion, setAcceptedVersion] = createSignal("");

  const items = createMemo(() => decodeSessionTimeline(timeline.data?.items ?? []));
  const diffFiles = createMemo(() => decodeSessionDiff(diff.data));
  const diffFileCount = createMemo(() => diffFiles().length);
  const asyncJobCount = createMemo(() => items().filter((item) => item.type === "job").length);
  const latestTurnId = createMemo(() => {
    const timelineItems = items();
    for (let index = timelineItems.length - 1; index >= 0; index -= 1) {
      const turnId = timelineItems[index]?.turnId;
      if (turnId) return turnId;
    }
    return undefined;
  });
  const currentTurnId = createMemo(() => session.data?.active_turn_id ?? latestTurnId());
  const latestTurn = useTurn(props.sessionId, currentTurnId);
  const durableAssistantRounds = createMemo(
    () =>
      new Set(
        items().flatMap((item) =>
          item.type === "assistant" && item.roundId ? [item.roundId] : [],
        ),
      ),
  );
  const provisionalOutput = createMemo(() => {
    const output = modelStreamOutput(props.sessionId(), currentTurnId());
    return isModelStreamOutputDurable(output, durableAssistantRounds()) ? null : output;
  });
  const active = createMemo(
    () => session.data?.state === "active" || Boolean(session.data?.active_turn_id),
  );
  const canMessage = createMemo(() =>
    session.data ? ["ready", "active"].includes(session.data.state) : false,
  );
  const status = () => session.data?.state ?? (session.isLoading ? "loading" : "unavailable");
  const subViews = createMemo<SessionSubView[]>(() => {
    const views: SessionSubView[] = ["main"];
    if (diffFileCount() > 0) views.push("diff");
    if (asyncJobCount() > 0) views.push("async");
    return views;
  });
  // If the active sub view is no longer offered (its data vanished), fall back.
  createEffect(() => {
    if (!subViews().includes(props.subView())) {
      props.onSubViewChange("main");
    }
  });

  createEffect(() => {
    const version = session.data?.version;
    if (version) setAcceptedVersion(version);
  });

  createEffect(() => {
    const turnId = currentTurnId();
    const status = latestTurn.data?.status;
    const output = modelStreamOutput(props.sessionId(), turnId);
    const hasDurableOutput = isModelStreamOutputDurable(output, durableAssistantRounds());
    if (
      hasDurableOutput ||
      status === "completed" ||
      status === "failed" ||
      status === "canceled" ||
      status === "interrupted" ||
      status === "handed_off"
    ) {
      clearModelStreamText(props.sessionId(), turnId);
      clearRetryState(props.sessionId(), turnId);
    }
  });

  createEffect(() => {
    const title = session.data?.title;
    if (title) props.onTitle?.(title);
  });

  async function sendMessage(
    content: string,
    modelPreference: SessionModelPreference | null,
    attachmentIds: readonly string[],
  ) {
    const version = acceptedVersion() || session.data?.version;
    if (!version) throw new Error("Session is not ready");

    const result = await postSessionMessage(props.sessionId(), {
      content,
      expected_session_version: version,
      model_preference: modelPreference,
      attachment_ids: [...attachmentIds],
    });
    setAcceptedVersion(result.session_version);
    // Let the model stream / turn.created SSE events drive the UI refresh
    // instead of a manual refetch. The explicit refetch forced a query
    // refetch window during which the conversation surface briefly blanked
    // (BUG 5); the SSE invalidation lands the new message without one.
    return { route: result.route };
  }

  const conversationError = () => {
    if (!session.isLoading && !session.data)
      return errorMessage(session.error, "Session not found");
    if (timeline.isError && items().length === 0) {
      return errorMessage(timeline.error, "Conversation failed to load");
    }
    return null;
  };

  const diffError = () => (diff.isError ? errorMessage(diff.error, "Diff failed to load") : null);

  return (
    <div class="session-doc">
      <header class="session-doc__toolbar">
        <nav class="session-subtabs" aria-label="Session views">
          <For each={subViews()}>
            {(view) => (
              <button
                type="button"
                class={`session-subtabs__item${props.subView() === view ? " session-subtabs__item--active" : ""}`}
                aria-pressed={props.subView() === view}
                onClick={() => props.onSubViewChange(view)}
              >
                <span class="session-subtabs__label">
                  {view === "main" ? "Main" : view === "diff" ? "Diff" : "Async"}
                </span>
                <Show when={view === "diff" && diffFileCount() > 0}>
                  <small class="session-subtabs__badge">{diffFileCount()}</small>
                </Show>
                <Show when={view === "async" && asyncJobCount() > 0}>
                  <small class="session-subtabs__badge">{asyncJobCount()}</small>
                </Show>
              </button>
            )}
          </For>
        </nav>
        <Badge variant={statusVariant(status())}>{status()}</Badge>
      </header>

      <div class="session-doc__view" hidden={props.subView() !== "main"}>
        <SessionConversation
          items={items()}
          loading={timeline.isLoading && items().length === 0}
          error={conversationError()}
          delivery={active() ? "queue" : "send"}
          composerDisabled={!canMessage()}
          contextUsage={context.data ?? null}
          limits={bootstrap.data?.data.limits}
          modelPreference={session.data?.model_preference ?? null}
          providers={providers.data ?? []}
          turn={latestTurn.data ?? null}
          provisionalText={provisionalOutput()?.text ?? ""}
          provisionalReasoning={provisionalOutput()?.reasoning ?? ""}
          sessionId={props.sessionId()}
          onRetry={() => {
            void session.refetch();
            void timeline.refetch();
          }}
          onSubmit={sendMessage}
          onUploadAttachment={uploadSessionAttachment}
          onDeleteAttachment={deleteSessionAttachment}
        />
      </div>

      <div class="session-doc__view" hidden={props.subView() !== "diff"}>
        <SessionDiffView
          files={diffFiles()}
          loading={diff.isLoading}
          error={diffError()}
          onRetry={() => void diff.refetch()}
        />
      </div>

      <div class="session-doc__view" hidden={props.subView() !== "async"}>
        <div class="session-async">
          <p class="session-async__title">Async jobs</p>
          <Show
            when={asyncJobCount() > 0}
            fallback={<p class="session-async__empty">No async jobs.</p>}
          >
            <For each={items().filter((item) => item.type === "job")}>
              {(item) => (item.type === "job" ? <JobCard item={item} /> : null)}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}

function statusVariant(status: string): BadgeVariant {
  if (status === "ready") return "success";
  if (["error", "failed", "unavailable"].includes(status)) return "danger";
  if (["active", "loading", "creating"].includes(status)) return "warning";
  return "neutral";
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}
