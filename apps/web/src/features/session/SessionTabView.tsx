import { useQueryClient } from "@tanstack/solid-query";
import { createEffect, createMemo, createSignal, For } from "solid-js";
import {
  ApiError,
  type ContextUsageView,
  cancelTurn,
  compactSession,
  deleteSessionAttachment,
  getErrorMessage,
  getSessionTimeline,
  postSessionMessage,
  type QueuedTurnItem,
  type SessionModelPreference,
  type SessionSummary,
  steerSession,
  type TimelinePage,
  uploadSessionAttachment,
} from "../../lib/api";
import {
  clearModelStreamText,
  clearModelStreamUsage,
  clearRetryState,
  isModelStreamOutputDurable,
  modelStreamOutput,
} from "../../lib/modelStream";
import {
  useAsyncTasks,
  useBootstrap,
  useProviders,
  useQueuedTurns,
  useSession,
  useSessionContext,
  useSessionTimeline,
  useSessionTimelineHistory,
  useTurn,
} from "../../lib/queries";
import { visibleTurnData } from "../../lib/queryPolicy";
import { renderSessionTurnId } from "../execution/sessionTurnState";
import { isTurnRunning } from "../execution/turnPresentation";
import { AsyncTasksView } from "./AsyncTasksView";
import { SessionConversation } from "./SessionConversation";
import { decodeSessionTimeline, type SessionTimelineItem } from "./sessionTimeline";
import { compressTimeline } from "./sessionTimelineCompression";
import "./session.css";

export type SessionSubView = "main" | "async";

interface SessionTabViewProps {
  sessionId: () => string;
  creating: () => boolean;
  subView: () => SessionSubView;
  onSubViewChange: (view: SessionSubView) => void;
  onTitle?: (title: string) => void;
}

export function SessionTabView(props: SessionTabViewProps) {
  const queryClient = useQueryClient();
  const queriesEnabled = () => {
    if (!props.creating()) return true;
    const cached = queryClient.getQueryData<SessionSummary>(["session", props.sessionId()]);
    return cached?.state !== "creating";
  };
  const session = useSession(props.sessionId, queriesEnabled);
  const bootstrap = useBootstrap();
  const timeline = useSessionTimeline(props.sessionId, queriesEnabled);
  const timelineHistory = useSessionTimelineHistory(props.sessionId, queriesEnabled);
  const context = useSessionContext(props.sessionId, queriesEnabled);
  const tasks = useAsyncTasks(queriesEnabled);
  const providers = useProviders();
  const [acceptedVersion, setAcceptedVersion] = createSignal("");
  const [pendingTurnId, setPendingTurnId] = createSignal<string | undefined>(undefined);
  const [pendingUserMessage, setPendingUserMessage] = createSignal<{
    turnId: string;
    text: string;
  } | null>(null);
  const [acceptedTurn, setAcceptedTurn] = createSignal<{
    id: string;
    route: string;
  } | null>(null);
  const [submittingMessage, setSubmittingMessage] = createSignal<string | null>(null);
  const [visibleTurnId, setVisibleTurnId] = createSignal<string | undefined>(undefined);
  let commandSerial = 0;
  let localSessionId = "";

  const [timelineSnapshot, setTimelineSnapshot] = createSignal<{
    sessionId: string;
    page: TimelinePage;
  } | null>(null);
  const timelineForSession = createMemo(() => {
    const page = timeline.data;
    const sessionId = props.sessionId();
    if (!page || page.items.some((item) => item.session_id !== sessionId)) return undefined;
    return page;
  });
  // Older pages fetched while scrolling up. Merged in front of the newest
  // window (dedup by item id, newest-window wins on conflict) so the
  // conversation renders one continuous document.
  const [loadingOlder, setLoadingOlder] = createSignal(false);
  const timelineWithHistory = createMemo(() => {
    const page = timelineForSession();
    const history = timelineHistory.data;
    if (!page) return undefined;
    if (!history || history.items.length === 0) return page;
    const newestIds = new Set(page.items.map((item) => item.id));
    const olderItems = history.items.filter((item) => !newestIds.has(item.id));
    const merged: TimelinePage = {
      ...page,
      items: [...olderItems, ...page.items],
      has_older: history.has_older,
    };
    if (history.oldest_cursor != null) merged.oldest_cursor = history.oldest_cursor;
    return merged;
  });
  const timelineHasOlder = createMemo(() => Boolean(timelineWithHistory()?.has_older));
  async function loadOlderTimeline() {
    const sessionId = props.sessionId();
    const page = timelineWithHistory();
    if (!sessionId || loadingOlder()) return;
    // Cursor to fetch before: the oldest item we currently hold.
    const oldest = page?.items[0]?.id;
    if (!oldest) return;
    setLoadingOlder(true);
    try {
      const older = await getSessionTimeline(sessionId, { before: oldest, limit: 100 });
      queryClient.setQueryData<TimelinePage | null>(
        ["session-timeline-history", sessionId],
        (current) => {
          const existing = current?.items ?? [];
          const existingIds = new Set(existing.map((item) => item.id));
          const merged = [...existing];
          for (const item of older.items) {
            if (!existingIds.has(item.id)) merged.push(item);
          }
          return {
            items: merged,
            has_older: older.has_older,
            has_newer: false,
            oldest_cursor: older.oldest_cursor ?? null,
            newest_cursor: current?.newest_cursor ?? null,
          };
        },
      );
    } finally {
      setLoadingOlder(false);
    }
  }

  // A tab can be reused for another Session. Local optimistic state belongs
  // to the old Session and must never select its Turn or message in the new
  // document while the first query is still settling.
  createEffect(() => {
    const sessionId = props.sessionId();
    if (sessionId === localSessionId) return;
    localSessionId = sessionId;
    commandSerial += 1;
    setAcceptedVersion("");
    setPendingTurnId(undefined);
    setPendingUserMessage(null);
    setAcceptedTurn(null);
    setVisibleTurnId(undefined);
    setTimelineSnapshot(null);
    // Old session's older pages must never bleed into the new one.
    queryClient.setQueryData<TimelinePage | null>(["session-timeline-history", sessionId], null);
  });

  createEffect(() => {
    const sessionId = props.sessionId();
    const page = timelineWithHistory();
    const snapshot = timelineSnapshot();
    if (snapshot?.sessionId !== sessionId) {
      if (page) setTimelineSnapshot({ sessionId, page });
      return;
    }
    if (page && page !== snapshot.page) setTimelineSnapshot({ sessionId, page });
  });
  const visibleTimelinePage = createMemo(() => {
    const sessionId = props.sessionId();
    const page = timelineWithHistory();
    const snapshot = timelineSnapshot();
    if (snapshot?.sessionId !== sessionId) return page;
    if (
      snapshot.page.items.length > 0 &&
      (!page || page.items.length === 0) &&
      session.data?.state !== "deleting"
    ) {
      return snapshot.page;
    }
    return page ?? snapshot.page;
  });
  let decodedSessionId = "";
  let previousDecodedItems: readonly SessionTimelineItem[] = [];
  const items = createMemo(() => {
    const sessionId = props.sessionId();
    if (sessionId !== decodedSessionId) {
      decodedSessionId = sessionId;
      previousDecodedItems = [];
    }
    const next = decodeSessionTimeline(visibleTimelinePage()?.items ?? [], previousDecodedItems);
    previousDecodedItems = next;
    return next;
  });
  let compressedSessionId = "";
  let previousConversationItems: readonly SessionTimelineItem[] = [];
  const conversationItems = createMemo(() => {
    const sessionId = props.sessionId();
    if (sessionId !== compressedSessionId) {
      compressedSessionId = sessionId;
      previousConversationItems = [];
    }
    const next = compressTimeline(items(), previousConversationItems);
    previousConversationItems = next;
    return next;
  });
  const asyncTaskCount = createMemo(() =>
    Math.max(tasks.data?.length ?? 0, items().filter((item) => item.type === "async_task").length),
  );
  const latestTurnId = createMemo(() => {
    const timelineItems = items();
    for (let index = timelineItems.length - 1; index >= 0; index -= 1) {
      const turnId = timelineItems[index]?.turnId;
      if (turnId) return turnId;
    }
    return undefined;
  });
  const activeTurnId = createMemo(() => session.data?.active_turn_id ?? undefined);
  const currentTurnId = createMemo(() =>
    renderSessionTurnId(
      visibleTurnId(),
      {
        active: activeTurnId(),
        pending: pendingTurnId(),
        latest: latestTurnId(),
      },
      acceptedTurn(),
    ),
  );
  const latestTurn = useTurn(props.sessionId, currentTurnId);
  const renderedTurn = createMemo(() => visibleTurnData(latestTurn.data, currentTurnId()) ?? null);
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
  // Thinking duration for the provisional Thought row. Prefer the server's
  // measured reasoning_duration_ms (pushed with the first answer delta) so the
  // label reads "Thought for Xs" the moment thinking completes and matches the
  // durable row; fall back to wall-clock while a server value is unavailable.
  const provisionalThinkingMs = createMemo(() => {
    const output = provisionalOutput();
    if (output?.reasoningDurationMs != null) return output.reasoningDurationMs;
    if (!output?.reasoningFirstSeenAt || !output.textFirstSeenAt) return null;
    return Math.max(0, output.textFirstSeenAt - output.reasoningFirstSeenAt);
  });
  const actionTurnId = createMemo(() => {
    const accepted = acceptedTurn();
    if (accepted?.route === "started") {
      return accepted.id;
    }
    return activeTurnId() ?? pendingTurnId();
  });
  const active = createMemo(() => Boolean(actionTurnId()) || isTurnRunning(renderedTurn()));
  const status = () => session.data?.state ?? (session.isLoading ? "loading" : "unavailable");
  const queuedTurns = useQueuedTurns(props.sessionId, () => actionTurnId() ?? null, queriesEnabled);

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
    const turnId = actionTurnId();
    const sid = props.sessionId();
    if (!turnId) return;
    const commandId = ++commandSerial;
    const idempotencyKey = crypto.randomUUID();
    const result = await withFreshSessionVersion(
      (version) => cancelTurn(sid, turnId, version, "user_cancel", idempotencyKey),
      (snapshot) => snapshot.active_turn_id === turnId,
    );
    if (commandId === commandSerial) {
      setAcceptedVersion(result.session_version);
      releaseLocalTurn(turnId);
    }
  }

  async function handleQueuedTurnCancel(turn: QueuedTurnItem) {
    const commandId = ++commandSerial;
    const idempotencyKey = crypto.randomUUID();
    const result = await withFreshSessionVersion(
      (version) =>
        cancelTurn(props.sessionId(), turn.turn_id, version, "user_cancel", idempotencyKey),
      async (snapshot) => {
        if (snapshot.active_turn_id === turn.turn_id) return false;
        const refreshed = await queuedTurns.refetch();
        return refreshed.data?.some((candidate) => candidate.turn_id === turn.turn_id) ?? false;
      },
    );
    if (commandId === commandSerial) {
      setAcceptedVersion(result.session_version);
      releaseLocalTurn(turn.turn_id);
    }
  }

  async function handleQueuedTurnSteer(turn: QueuedTurnItem) {
    const activeTurn = actionTurnId();
    if (!activeTurn) throw new Error("There is no active turn to steer");
    const commandId = ++commandSerial;
    const steerKey = crypto.randomUUID();
    const steer = await withFreshSessionVersion(
      (version) => steerSession(props.sessionId(), turn.message_text, version, steerKey),
      (snapshot) => snapshot.active_turn_id === activeTurn,
    );
    if (commandId !== commandSerial) return;
    setAcceptedVersion(steer.session_version);

    // The queued message has now been consumed as a durable steer. Remove the
    // queued Turn with a second idempotent command so it cannot execute again.
    const cancelKey = crypto.randomUUID();
    const canceled = await withFreshSessionVersion(
      (version) => cancelTurn(props.sessionId(), turn.turn_id, version, "steered", cancelKey),
      async (snapshot) => {
        if (snapshot.active_turn_id === turn.turn_id) return false;
        const refreshed = await queuedTurns.refetch();
        return refreshed.data?.some((candidate) => candidate.turn_id === turn.turn_id) ?? false;
      },
    );
    if (commandId === commandSerial) setAcceptedVersion(canceled.session_version);
  }
  const canMessage = createMemo(() =>
    session.data ? ["ready", "active"].includes(session.data.state) : false,
  );
  const subViews = createMemo<SessionSubView[]>(() => {
    const views: SessionSubView[] = ["main"];
    if (asyncTaskCount() > 0) views.push("async");
    return views;
  });
  // If the active sub view is no longer offered (its data vanished), fall back.
  createEffect(() => {
    const turnId = currentTurnId();
    if (turnId && turnId !== visibleTurnId()) setVisibleTurnId(turnId);
  });

  createEffect(() => {
    if (!subViews().includes(props.subView())) {
      props.onSubViewChange("main");
    }
  });

  createEffect(() => {
    const version = session.data?.version;
    // A command response can arrive before the invalidated Session query. Do
    // not overwrite that newer version with the query's stale snapshot; a
    // later 412 refresh still repairs an out-of-band change.
    if (version && !acceptedVersion()) setAcceptedVersion(version);
  });

  createEffect(() => {
    const turnId = currentTurnId();
    const status = renderedTurn()?.status;
    const output = modelStreamOutput(props.sessionId(), turnId);
    const hasDurableOutput = isModelStreamOutputDurable(output, durableAssistantRounds());
    const terminal = ["completed", "failed", "canceled", "interrupted"].includes(status ?? "");
    if (hasDurableOutput || (terminal && output === null)) {
      clearModelStreamText(props.sessionId(), turnId);
      if (terminal) clearModelStreamUsage(props.sessionId(), turnId);
      clearRetryState(props.sessionId(), turnId);
    }
  });

  createEffect(() => {
    const pending = pendingTurnId();
    const turn = renderedTurn();
    const status = turn?.status;
    const output = modelStreamOutput(props.sessionId(), pending);
    const outputIsDurable = isModelStreamOutputDurable(output, durableAssistantRounds());
    const hasDurableTimelineItem = Boolean(
      pending && items().some((item) => item.turnId === pending),
    );
    if (
      pending &&
      turn?.id === pending &&
      hasDurableTimelineItem &&
      ["completed", "failed", "canceled", "interrupted"].includes(status ?? "") &&
      (output === null || outputIsDurable)
    ) {
      setPendingTurnId(undefined);
      setAcceptedTurn(null);
      setPendingUserMessage(null);
    }
  });

  createEffect(() => {
    const provisional = pendingUserMessage();
    if (!provisional) return;
    if (items().some((item) => item.type === "user" && item.turnId === provisional.turnId)) {
      setPendingUserMessage(null);
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
    goalMode: boolean,
  ) {
    const commandId = ++commandSerial;
    const idempotencyKey = crypto.randomUUID();
    setSubmittingMessage(content);
    try {
      const result = await withFreshSessionVersion((version) =>
        postSessionMessage(
          props.sessionId(),
          {
            content,
            expected_session_version: version,
            model_preference: modelPreference,
            attachment_ids: [...attachmentIds],
            goal_mode: goalMode,
          },
          idempotencyKey,
        ),
      );
      if (commandId === commandSerial) {
        setAcceptedVersion(result.session_version);
        setAcceptedTurn({ id: result.turn_id, route: result.route });
        setPendingTurnId(result.turn_id);
        // The provisional user bubble is only for turns that started running
        // immediately. A queued turn has no live bubble — it renders in
        // QueuedMessagesBar until it is consumed (or cancelled).
        if (result.route === "started") {
          setPendingUserMessage({ turnId: result.turn_id, text: content });
          setVisibleTurnId(result.turn_id);
        }
      }
      return { route: result.route, turnId: result.turn_id };
    } finally {
      setSubmittingMessage(null);
    }
  }

  async function compactContext() {
    const sessionId = props.sessionId();
    const operation = await compactSession(sessionId, crypto.randomUUID());
    queryClient.setQueryData(["operations", operation.id], operation);
    if (operation.status === "queued") {
      queryClient.setQueryData<ContextUsageView | null>(
        ["session-context", sessionId],
        (current) => (current ? { ...current, compact_status: "scheduled" } : current),
      );
    }
  }

  function releaseLocalTurn(turnId: string) {
    const accepted = acceptedTurn();
    const pending = pendingTurnId();
    if (accepted && accepted.id !== turnId) return;
    if (pending && pending !== turnId) return;
    setPendingTurnId(undefined);
    setAcceptedTurn(null);
    // Keep the provisional user bubble until the timeline contains the
    // durable input. This avoids a visible disappearance during cancellation.
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
                <span class="session-subtabs__label">{view === "main" ? "Main" : "Async"}</span>
              </button>
            )}
          </For>
        </nav>
      </header>

      <div class="session-doc__view" hidden={props.subView() !== "main"}>
        <SessionConversation
          items={conversationItems()}
          loading={
            status() === "loading" ||
            (!props.creating() && timeline.isLoading && items().length === 0)
          }
          error={conversationError()}
          delivery={active() ? "queue" : "send"}
          composerDisabled={!canMessage()}
          composerSettingsDisabled={!props.creating() && !session.data}
          hasOlder={timelineHasOlder()}
          loadingOlder={loadingOlder()}
          onLoadOlder={loadOlderTimeline}
          onCancel={active() ? handleCancel : undefined}
          queuedTurns={queuedTurns.data ?? []}
          onQueuedTurnCancel={handleQueuedTurnCancel}
          onQueuedTurnSteer={active() ? handleQueuedTurnSteer : undefined}
          contextUsage={context.data ?? null}
          limits={bootstrap.data?.data.limits}
          modelPreference={session.data?.model_preference ?? null}
          providers={providers.data ?? []}
          turn={renderedTurn()}
          provisionalUserTurnId={pendingUserMessage()?.turnId ?? null}
          provisionalUserText={pendingUserMessage()?.text ?? submittingMessage() ?? ""}
          provisionalText={provisionalOutput()?.text ?? ""}
          provisionalReasoning={provisionalOutput()?.reasoning ?? ""}
          provisionalThinkingMs={provisionalThinkingMs()}
          provisionalRoundId={provisionalOutput()?.roundId ?? null}
          sessionId={props.sessionId()}
          onRetry={() => {
            void session.refetch();
            void timeline.refetch();
          }}
          onSubmit={sendMessage}
          onUploadAttachment={uploadSessionAttachment}
          onDeleteAttachment={deleteSessionAttachment}
          onCompact={compactContext}
        />
      </div>

      <div class="session-doc__view" hidden={props.subView() !== "async"}>
        <div class="session-async">
          <p class="session-async__title">Async tasks</p>
          <AsyncTasksView
            tasks={tasks.data ?? []}
            loading={tasks.isLoading}
            error={
              tasks.isError ? getErrorMessage(tasks.error, "Async tasks failed to load") : null
            }
            onRefresh={() => void tasks.refetch()}
          />
        </div>
      </div>
    </div>
  );
}

function errorMessage(error: unknown, fallback: string): string {
  return getErrorMessage(error, fallback);
}
