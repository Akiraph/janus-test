import { useQueryClient } from "@tanstack/solid-query";
import { ArrowDownToLine, ArrowUpFromLine, Loader2, TriangleAlert } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Alt } from "../../../components/ui/Alt";
import { Button } from "../../../components/ui/Button";
import {
  ApiError,
  type AskAnswer,
  answerAsk,
  applySession,
  cancelTurn,
  deleteSessionAttachment,
  getErrorMessage,
  postSessionMessage,
  type QueuedTurnItem,
  type SessionModelPreference,
  type SessionSummary,
  syncSession,
  type TimelinePage,
  uploadSessionAttachment,
} from "../../../lib/api";
import { clearRetryState } from "../../../lib/modelRetryState";
import {
  clearModelStreamText,
  clearModelStreamUsage,
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
import { visibleTurnData } from "../../../lib/queryPolicy";
import { JobCard } from "./SessionCards";
import { SessionConversation } from "./SessionConversation";
import { SessionDiffView } from "./SessionDiffView";
import { decodeSessionDiff } from "./sessionDiff";
import { decodeSessionTimeline, type SessionTimelineItem } from "./sessionTimeline";
import { compressTimeline } from "./sessionTimelineCompression";
import { renderSessionTurnId } from "./sessionTurnState";
import "./session.css";

export type SessionSubView = "main" | "diff" | "async";

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
  const context = useSessionContext(props.sessionId, queriesEnabled);
  const providers = useProviders();
  const [diffRequested, setDiffRequested] = createSignal(false);
  let diffLoadTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    props.sessionId();
    setDiffRequested(false);
    if (diffLoadTimer !== undefined) clearTimeout(diffLoadTimer);
    diffLoadTimer = setTimeout(() => setDiffRequested(true), 250);
    onCleanup(() => {
      if (diffLoadTimer !== undefined) clearTimeout(diffLoadTimer);
      diffLoadTimer = undefined;
    });
  });
  createEffect(() => {
    if (props.subView() === "diff") setDiffRequested(true);
  });
  const diff = useSessionDiff(props.sessionId, () => queriesEnabled() && diffRequested());
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
  const [propagationAction, setPropagationAction] = createSignal<"sync" | "apply" | null>(null);
  const [propagationError, setPropagationError] = createSignal<string | null>(null);
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
  });

  createEffect(() => {
    const sessionId = props.sessionId();
    const page = timelineForSession();
    const snapshot = timelineSnapshot();
    if (snapshot?.sessionId !== sessionId) {
      if (page) setTimelineSnapshot({ sessionId, page });
      return;
    }
    if (page && page !== snapshot.page) setTimelineSnapshot({ sessionId, page });
  });
  const visibleTimelinePage = createMemo(() => {
    const sessionId = props.sessionId();
    const page = timelineForSession();
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
  const diffModel = createMemo(() => decodeSessionDiff(diff.data));
  const diffFiles = createMemo(() => diffModel().files);
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
  const actionTurnId = createMemo(() => {
    const accepted = acceptedTurn();
    if (accepted && (accepted.route === "started" || accepted.route === "handed_off")) {
      return accepted.id;
    }
    return activeTurnId() ?? pendingTurnId();
  });
  const active = createMemo(() => Boolean(actionTurnId()));
  const status = () => session.data?.state ?? (session.isLoading ? "loading" : "unavailable");
  const queuedTurns = useQueuedTurns(props.sessionId, () => actionTurnId() ?? null, queriesEnabled);
  const canPropagate = createMemo(
    () => status() === "ready" && !active() && propagationAction() === null,
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
    const turnId = actionTurnId();
    const sid = props.sessionId();
    if (!turnId) return;
    const commandId = ++commandSerial;
    const result = await withFreshSessionVersion(
      (version) => cancelTurn(sid, turnId, version),
      (snapshot) => snapshot.active_turn_id === turnId,
    );
    if (commandId === commandSerial) {
      setAcceptedVersion(result.session_version);
      releaseLocalTurn(turnId);
    }
    void session.refetch();
    void timeline.refetch();
    void queuedTurns.refetch();
  }

  async function handleQueuedTurnCancel(turn: QueuedTurnItem) {
    const commandId = ++commandSerial;
    const result = await withFreshSessionVersion(
      (version) => cancelTurn(props.sessionId(), turn.turn_id, version),
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
    void session.refetch();
    void timeline.refetch();
    void queuedTurns.refetch();
  }
  const canMessage = createMemo(() =>
    session.data ? ["ready", "active"].includes(session.data.state) : false,
  );
  const subViews = createMemo<SessionSubView[]>(() => {
    const views: SessionSubView[] = ["main"];
    if (
      diffFileCount() > 0 ||
      diffModel().syncEnabled ||
      diffModel().applyEnabled ||
      diffModel().pendingConflict
    ) {
      views.push("diff");
    }
    if (asyncJobCount() > 0) views.push("async");
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
    const terminal = ["completed", "failed", "canceled", "interrupted", "handed_off"].includes(
      status ?? "",
    );
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
      ["completed", "failed", "canceled", "interrupted", "handed_off"].includes(status ?? "") &&
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
  ) {
    const commandId = ++commandSerial;
    setSubmittingMessage(content);
    try {
      const result = await withFreshSessionVersion((version) =>
        postSessionMessage(props.sessionId(), {
          content,
          expected_session_version: version,
          model_preference: modelPreference,
          attachment_ids: [...attachmentIds],
        }),
      );
      if (commandId === commandSerial) {
        setAcceptedVersion(result.session_version);
        setAcceptedTurn({ id: result.turn_id, route: result.route });
        setPendingTurnId(result.turn_id);
        setPendingUserMessage({ turnId: result.turn_id, text: content });
        if (result.route === "started" || result.route === "handed_off") {
          setVisibleTurnId(result.turn_id);
        }
      }
      // SSE normally invalidates these queries, but the POST response is the
      // first authoritative signal when the stream is reconnecting or its
      // cursor is being repaired. Refresh immediately so the active Turn
      // starts polling even when no event reaches this tab.
      void Promise.allSettled([session.refetch(), timeline.refetch(), queuedTurns.refetch()]);
      return { route: result.route, turnId: result.turn_id };
    } finally {
      setSubmittingMessage(null);
    }
  }

  async function answerAskFromConversation(askId: string, answer: AskAnswer) {
    const commandId = ++commandSerial;
    const result = await answerAsk(askId, answer);
    if (commandId === commandSerial) {
      setAcceptedVersion(result.session_version);
      const route = result.route_or_status;
      if (route === "queued") {
        setAcceptedTurn({ id: result.turn_id, route });
        setPendingTurnId(result.turn_id);
      } else if (!["answered", "canceled", "expired", "closed_by_handoff"].includes(route)) {
        // Accepted answers resume the existing Turn (`running`) or create a
        // late-answer successor (`started`/`handed_off`). Treat all of those
        // as an active command until the authoritative Turn query settles.
        setAcceptedTurn({ id: result.turn_id, route: "started" });
        setVisibleTurnId(result.turn_id);
      }
    }
    void session.refetch();
    void timeline.refetch();
    void queuedTurns.refetch();
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

  async function runPropagation(direction: "sync" | "apply") {
    if (!canPropagate()) return;
    setPropagationAction(direction);
    setPropagationError(null);
    try {
      if (direction === "sync") {
        await syncSession(props.sessionId());
      } else {
        await applySession(props.sessionId());
      }
      await diff.refetch();
      void session.refetch();
    } catch (error) {
      setPropagationError(getErrorMessage(error, `${direction} failed`));
      await diff.refetch();
    } finally {
      setPropagationAction(null);
    }
  }

  async function handleResolve() {
    const conflict = diffModel().pendingConflict;
    if (!conflict || !canPropagate() || !canMessage()) return;
    const paths = conflict.paths
      .map(
        (path) =>
          `- ${path.path} (${path.kind}; main ${path.mainHash ?? "missing"}, session ${path.sessionHash ?? "missing"})`,
      )
      .join("\n");
    const prompt = [
      "The workspace has a propagation conflict.",
      `The conflict was detected during ${conflict.direction}.`,
      "Please inspect and edit the session workspace so these files contain the intended merged result:",
      paths,
      "Do not commit the changes. Tell me when the workspace is resolved; I will apply it after this turn.",
    ].join("\n\n");
    try {
      await sendMessage(prompt, session.data?.model_preference ?? null, []);
      setPropagationError(null);
    } catch (error) {
      setPropagationError(getErrorMessage(error, "Resolve turn failed"));
    }
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
              </button>
            )}
          </For>
        </nav>
        <div class="session-doc__toolbar-actions">
          <Show when={subViews().includes("diff")}>
            <Button
              variant="outline"
              size="sm"
              disabled={!diffModel().syncEnabled || !canPropagate()}
              onClick={() => void runPropagation("sync")}
            >
              <Show when={propagationAction() === "sync"} fallback={<ArrowDownToLine size={14} />}>
                <Loader2 size={14} class="ui-spinner" />
              </Show>
              Sync
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={!diffModel().applyEnabled || !canPropagate()}
              onClick={() => void runPropagation("apply")}
            >
              <Show when={propagationAction() === "apply"} fallback={<ArrowUpFromLine size={14} />}>
                <Loader2 size={14} class="ui-spinner" />
              </Show>
              Apply
            </Button>
            <Show when={diffModel().pendingConflict}>
              {(conflict) => (
                <Alt
                  interactive
                  class="alt-bubble--propagation"
                  content={
                    <div class="session-propagation-conflict-popover">
                      <strong>Conflict</strong>
                      <p>
                        Resolve {conflict().paths.length} file
                        {conflict().paths.length === 1 ? "" : "s"} in a new turn before applying.
                      </p>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={!canPropagate()}
                        onClick={() => void handleResolve()}
                      >
                        Resolve
                      </Button>
                    </div>
                  }
                >
                  <button
                    type="button"
                    class="session-propagation-conflict"
                    aria-label="Workspace conflict"
                  >
                    <TriangleAlert size={15} />
                  </button>
                </Alt>
              )}
            </Show>
          </Show>
        </div>
      </header>

      <div class="session-doc__view" hidden={props.subView() !== "main"}>
        <SessionConversation
          items={conversationItems()}
          loading={
            status() === "creating" ||
            status() === "loading" ||
            (timeline.isLoading && items().length === 0)
          }
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
          turn={renderedTurn()}
          provisionalUserTurnId={pendingUserMessage()?.turnId ?? null}
          provisionalUserText={pendingUserMessage()?.text ?? submittingMessage() ?? ""}
          provisionalText={provisionalOutput()?.text ?? ""}
          provisionalReasoning={provisionalOutput()?.reasoning ?? ""}
          provisionalRoundId={provisionalOutput()?.roundId ?? null}
          sessionId={props.sessionId()}
          onRetry={() => {
            void session.refetch();
            void timeline.refetch();
          }}
          onSubmit={sendMessage}
          onAnswer={answerAskFromConversation}
          onUploadAttachment={uploadSessionAttachment}
          onDeleteAttachment={deleteSessionAttachment}
        />
      </div>

      <div class="session-doc__view" hidden={props.subView() !== "diff"}>
        <SessionDiffView
          files={diffFiles()}
          loading={diff.isLoading}
          error={diffError()}
          actionError={propagationError()}
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

function errorMessage(error: unknown, fallback: string): string {
  return getErrorMessage(error, fallback);
}
