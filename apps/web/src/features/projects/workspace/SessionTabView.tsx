import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { Badge, type BadgeVariant } from "../../../components/ui/Badge";
import {
  ApiError,
  cancelTurn,
  deleteSessionAttachment,
  getErrorMessage,
  postSessionMessage,
  type QueuedTurnItem,
  type SessionModelPreference,
  type SessionSummary,
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
  useQueuedTurns,
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
  const [pendingTurnId, setPendingTurnId] = createSignal<string | undefined>(undefined);

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
  const activeTurnId = createMemo(() => session.data?.active_turn_id ?? undefined);
  const currentTurnId = createMemo(() => activeTurnId() ?? pendingTurnId() ?? latestTurnId());
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
  const active = createMemo(() => Boolean(activeTurnId() ?? pendingTurnId()));
  const queuedTurns = useQueuedTurns(
    props.sessionId,
    () => activeTurnId() ?? pendingTurnId() ?? null,
  );

  async function withFreshSessionVersion<T>(
    command: (version: string) => Promise<T>,
    canRetry: (snapshot: SessionSummary) => boolean | Promise<boolean> = () => true,
  ): Promise<T> {
    const initialVersion = acceptedVersion() || session.data?.version;
    if (!initialVersion) throw new Error("Session is not ready");

    try {
      return await command(initialVersion);
    } catch (error) {
      if (!(error instanceof ApiError) || error.status !== 412) throw error;
      const refreshed = await session.refetch();
      const snapshot = refreshed.data;
      if (!snapshot || snapshot.version === initialVersion || !(await canRetry(snapshot))) {
        throw error;
      }
      setAcceptedVersion(snapshot.version);
      return command(snapshot.version);
    }
  }

  async function handleCancel() {
    const turnId = activeTurnId() ?? pendingTurnId();
    const sid = props.sessionId();
    if (!turnId) return;
    const result = await withFreshSessionVersion(
      (version) => cancelTurn(sid, turnId, version),
      (snapshot) => snapshot.active_turn_id === turnId,
    );
    setAcceptedVersion(result.session_version);
    void session.refetch();
    void timeline.refetch();
    void queuedTurns.refetch();
  }

  async function handleQueuedTurnCancel(turn: QueuedTurnItem) {
    const result = await withFreshSessionVersion(
      (version) => cancelTurn(props.sessionId(), turn.turn_id, version),
      async (snapshot) => {
        if (snapshot.active_turn_id === turn.turn_id) return false;
        const refreshed = await queuedTurns.refetch();
        return refreshed.data?.some((candidate) => candidate.turn_id === turn.turn_id) ?? false;
      },
    );
    setAcceptedVersion(result.session_version);
    void session.refetch();
    void timeline.refetch();
    void queuedTurns.refetch();
  }
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
    const terminal = ["completed", "failed", "canceled", "interrupted", "handed_off"].includes(
      status ?? "",
    );
    if (hasDurableOutput || (terminal && output === null)) {
      clearModelStreamText(props.sessionId(), turnId);
      clearRetryState(props.sessionId(), turnId);
    }
  });

  createEffect(() => {
    const pending = pendingTurnId();
    const status = latestTurn.data?.status;
    const output = modelStreamOutput(props.sessionId(), pending);
    const outputIsDurable = isModelStreamOutputDurable(output, durableAssistantRounds());
    if (
      pending &&
      latestTurn.data?.id === pending &&
      ["completed", "failed", "canceled", "interrupted", "handed_off"].includes(status ?? "") &&
      (output === null || outputIsDurable)
    ) {
      setPendingTurnId(undefined);
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
    const result = await withFreshSessionVersion((version) =>
      postSessionMessage(props.sessionId(), {
        content,
        expected_session_version: version,
        model_preference: modelPreference,
        attachment_ids: [...attachmentIds],
      }),
    );
    setAcceptedVersion(result.session_version);
    setPendingTurnId(result.turn_id);
    // Let the model stream / turn.created SSE events drive the UI refresh
    // instead of a manual refetch. The explicit refetch forced a query
    // refetch window during which the conversation surface briefly blanked
    // (BUG 5); the SSE invalidation lands the new message without one.
    return { route: result.route, turnId: result.turn_id };
  }

  const conversationError = () => {
    if (session.data?.state === "creating") return null;
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
          onCancel={active() ? handleCancel : undefined}
          queuedTurns={queuedTurns.data ?? []}
          onQueuedTurnCancel={handleQueuedTurnCancel}
          contextUsage={context.data ?? null}
          limits={bootstrap.data?.data.limits}
          modelPreference={session.data?.model_preference ?? null}
          providers={providers.data ?? []}
          turn={latestTurn.data ?? null}
          provisionalText={provisionalOutput()?.text ?? ""}
          provisionalReasoning={provisionalOutput()?.reasoning ?? ""}
          provisionalRoundId={provisionalOutput()?.roundId ?? null}
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
  return getErrorMessage(error, fallback);
}
