import { Loader2 } from "lucide-react";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { Button } from "../../../components/ui/button";
import {
  latestTodoFromRuns,
  toCliJobViews,
  toDiscussionViews,
  toSessionConversationItems,
} from "../data/mappers";
import { useAnswerSupervisorAsk } from "../hooks/useAnswerSupervisorAsk";
import { useBranchSession } from "../hooks/useBranchSession";
import { useCancelSupervisorRun } from "../hooks/useCancelSupervisorRun";
import { useDeliverQueuedSupervisorRun } from "../hooks/useDeliverQueuedSupervisorRun";
import { useSessionActivity } from "../hooks/useSessionActivity";
import { useSessionCheckpoints } from "../hooks/useSessionCheckpoints";
import { useStartSupervisorRun } from "../hooks/useStartSupervisorRun";
import { useSupervisorRuns } from "../hooks/useSupervisorRuns";
import {
  ActiveRunStatusOutput,
  latestSupervisorModelRetryEvent,
} from "./ActiveRunStatusOutput";
import { ApplyReviewPanel } from "./ApplyReviewPanel";
import { CliJobsSection } from "./CliJobsSection";
import { ConversationItemRow } from "./ConversationItemRow";
import {
  activeSessionRuns,
  applyCheckpointVersionSelections,
  buildCheckpointVersionGraph,
  pendingAskRequests,
  withVersionNavigation,
} from "./conversation-run-selectors";
import { GroupDiscussionsSection } from "./GroupDiscussionsSection";
import { PendingAskCard } from "./PendingAskCard";
import { PromptInput, type PromptSubmitOptions } from "./PromptInput";
import { QueuedMessagesBar } from "./QueuedMessagesBar";
import { latestRunDiff, SessionChangesSection } from "./SessionChangesSection";
import { type SessionRailSection, SessionRightRail } from "./SessionRightRail";
import { TodoSection } from "./TodoSection";

interface ConversationViewProps {
  sessionId: string;
  projectId: string;
  onOpenSession?: ((sessionId: string) => void) | undefined;
  onOpenSessionDiff?: ((sessionId: string) => void) | undefined;
}

const bottomFollowThresholdPx = 32;

export function ConversationView({
  sessionId,
  projectId,
  onOpenSession,
  onOpenSessionDiff,
}: ConversationViewProps) {
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const shouldFollowBottomRef = useRef(true);
  const {
    data: runsData,
    isLoading,
    error,
    refetch,
  } = useSupervisorRuns(sessionId);
  const startRun = useStartSupervisorRun();
  const branchSession = useBranchSession();
  const cancelRun = useCancelSupervisorRun(sessionId);
  const deliverQueuedRun = useDeliverQueuedSupervisorRun(sessionId);
  const answerAsk = useAnswerSupervisorAsk(sessionId);
  const { data: checkpointsData } = useSessionCheckpoints(sessionId);
  const [selectedCliJobId, setSelectedCliJobId] = useState<
    string | undefined
  >();
  const [selectedVersionState, setSelectedVersionState] = useState<{
    readonly sessionId: string;
    readonly runIds: Record<string, string>;
  }>({ sessionId, runIds: {} });
  const [railFocusSequence, setRailFocusSequence] = useState(0);
  const [editingUserMessage, setEditingUserMessage] = useState<
    | {
        readonly runId: string;
        readonly originalText: string;
        readonly draft: string;
      }
    | undefined
  >();
  const [actionError, setActionError] = useState<string | undefined>();

  const runs = runsData?.runs ?? [];
  const checkpoints = checkpointsData?.checkpoints ?? [];
  const selectedVersionRunIds =
    selectedVersionState.sessionId === sessionId
      ? selectedVersionState.runIds
      : {};
  const versionGraph = useMemo(
    () => buildCheckpointVersionGraph(checkpoints),
    [checkpoints],
  );
  const cliJobs = useMemo(() => toCliJobViews(runs), [runs]);
  const discussions = useMemo(() => toDiscussionViews(runs), [runs]);
  const latestTodo = useMemo(() => latestTodoFromRuns(runs), [runs]);
  const pendingAsks = useMemo(() => pendingAskRequests(runs), [runs]);
  const activeRuns = useMemo(() => activeSessionRuns(runs), [runs]);
  const currentRun =
    activeRuns.find((run) => run.status === "running") ?? activeRuns[0];
  const activityQuery = useSessionActivity(
    sessionId,
    projectId,
    currentRun !== undefined,
  );
  const latestRetryEvent = useMemo(
    () =>
      latestSupervisorModelRetryEvent(
        activityQuery.data?.events ?? [],
        currentRun,
      ),
    [activityQuery.data?.events, currentRun],
  );
  const queuedMessageRuns = activeRuns.filter(
    (run) => run.status === "queued" && run.id !== currentRun?.id,
  );
  const queuedMessageRunIds = useMemo(
    () => new Set(queuedMessageRuns.map((run) => run.id)),
    [queuedMessageRuns],
  );
  const visibleRuns = useMemo(() => {
    const nonQueuedRuns = runs.filter(
      (run) => !queuedMessageRunIds.has(run.id),
    );
    return applyCheckpointVersionSelections(
      nonQueuedRuns,
      versionGraph,
      selectedVersionRunIds,
    );
  }, [queuedMessageRunIds, runs, selectedVersionRunIds, versionGraph]);
  const latestDiff = useMemo(() => latestRunDiff(runs), [runs]);
  const conversationItems = useMemo(
    () =>
      withVersionNavigation(
        toSessionConversationItems(visibleRuns),
        versionGraph,
        selectedVersionRunIds,
      ),
    [selectedVersionRunIds, versionGraph, visibleRuns],
  );
  const railSections: SessionRailSection[] = [];
  railSections.push({
    id: "changes",
    title: "Changes",
    content: (
      <SessionChangesSection
        diff={latestDiff}
        loading={isLoading}
        onOpenDiff={() => onOpenSessionDiff?.(sessionId)}
      />
    ),
  });
  if (latestTodo !== undefined && latestTodo.length > 0) {
    railSections.push({
      id: "todo",
      title: "Todo",
      content: <TodoSection items={latestTodo} />,
    });
  }
  if (discussions.length > 0) {
    railSections.push({
      id: "discussions",
      title: "Group discussions",
      content: <GroupDiscussionsSection discussions={discussions} />,
    });
  }
  if (cliJobs.length > 0) {
    railSections.push({
      id: "cli-jobs",
      title: "CLI jobs",
      content: (
        <CliJobsSection
          jobs={cliJobs}
          onCancelJob={(runId) => cancelRun.mutate(runId)}
          cancelling={cancelRun.isPending}
          selectedJobId={selectedCliJobId}
          onSelectedJobChange={setSelectedCliJobId}
        />
      ),
    });
  }
  railSections.push({
    id: "apply-review",
    title: "Apply",
    content: <ApplyReviewPanel sessionId={sessionId} projectId={projectId} />,
  });
  const railContentKey = [
    sessionId,
    latestTodo
      ?.map((item) => `${item.id}:${item.status}:${item.content}`)
      .join("|") ?? "",
    discussions
      .map(
        (discussion) =>
          `${discussion.id}:${discussion.status}:${discussion.completedAt ?? ""}:${discussion.participants
            .map((participant) => participant.status)
            .join(",")}`,
      )
      .join("|"),
    cliJobs
      .map((job) => `${job.id}:${job.status}:${job.completedAt ?? ""}`)
      .join("|"),
    latestDiff === undefined
      ? ""
      : `${latestDiff.updatedAt}:${latestDiff.files.length}:${latestDiff.patch.length}`,
  ].join("::");

  // biome-ignore lint/correctness/useExhaustiveDependencies: session changes should reset the scroll anchor even though the effect only touches refs.
  useLayoutEffect(() => {
    shouldFollowBottomRef.current = true;
    scrollToConversationBottom(scrollContainerRef.current);
  }, [sessionId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: these derived values are the render changes that should trigger follow-bottom scrolling.
  useLayoutEffect(() => {
    if (shouldFollowBottomRef.current) {
      scrollToConversationBottom(scrollContainerRef.current);
    }
  }, [conversationItems, currentRun, latestRetryEvent]);

  const handleScroll = () => {
    const scrollContainer = scrollContainerRef.current;

    if (scrollContainer === null) {
      return;
    }

    shouldFollowBottomRef.current =
      scrollContainer.scrollHeight -
        scrollContainer.scrollTop -
        scrollContainer.clientHeight <=
      bottomFollowThresholdPx;
  };

  const handleSendMessage = async (
    message: string,
    options: PromptSubmitOptions,
  ) => {
    setActionError(undefined);
    await startRun.mutateAsync({
      projectId,
      sessionId,
      task: message,
      deliveryIntent: options.deliveryIntent,
      attachments: options.attachments,
    });
  };

  const handleCancelRun = () => {
    if (currentRun !== undefined) {
      cancelRun.mutate(currentRun.id);
    }
  };

  const handleDeliverQueuedRun = (runId: string) => {
    deliverQueuedRun.mutate(runId);
  };

  const handleDeleteQueuedRun = (runId: string) => {
    cancelRun.mutate(runId);
  };

  const handleSelectCliJob = (jobId: string) => {
    setSelectedCliJobId(jobId);
    setRailFocusSequence((value) => value + 1);
  };

  const handleNavigateUserMessageVersion = ({
    parentRunId,
    runId,
  }: {
    readonly parentRunId: string;
    readonly runId: string;
  }) => {
    setSelectedVersionState((current) => ({
      sessionId,
      runIds: {
        ...(current.sessionId === sessionId ? current.runIds : {}),
        [parentRunId]: runId,
      },
    }));
  };

  const handleCopyUserMessage = (text: string) => {
    void navigator.clipboard?.writeText(text);
  };

  const handleBranchFromRun = async (runId: string) => {
    setActionError(undefined);
    try {
      const response = await branchSession.mutateAsync({
        projectId,
        sessionId,
        runId,
        origin: "branch",
      });
      onOpenSession?.(response.session.id);
    } catch (error) {
      console.error("Failed to branch session:", error);
      setActionError(errorMessage(error, "Failed to branch session."));
    }
  };

  const handleStartEditFromRun = ({
    runId,
    text,
  }: {
    readonly runId: string;
    readonly text: string;
  }) => {
    setActionError(undefined);
    setEditingUserMessage({ runId, originalText: text, draft: text });
  };

  const handleCancelEdit = () => {
    setEditingUserMessage(undefined);
  };

  const handleSaveEdit = async () => {
    if (editingUserMessage === undefined) {
      return;
    }

    const edited = editingUserMessage.draft.trim();

    if (
      edited.length === 0 ||
      edited === editingUserMessage.originalText.trim()
    ) {
      setEditingUserMessage(undefined);
      return;
    }

    setActionError(undefined);
    try {
      const sourceCheckpoint = checkpoints.find(
        (checkpoint) => checkpoint.runId === editingUserMessage.runId,
      );
      const response = await branchSession.mutateAsync({
        projectId,
        sessionId,
        ...(sourceCheckpoint?.parentCheckpointId === undefined
          ? { runId: editingUserMessage.runId }
          : { checkpointId: sourceCheckpoint.parentCheckpointId }),
        origin: "rewind",
      });
      onOpenSession?.(response.session.id);
      await startRun.mutateAsync({
        projectId,
        sessionId: response.session.id,
        task: edited,
      });
      setEditingUserMessage(undefined);
    } catch (error) {
      console.error("Failed to rewind from message:", error);
      setActionError(errorMessage(error, "Failed to edit message."));
    }
  };

  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="flex min-h-0 flex-1">
        <div
          ref={scrollContainerRef}
          onScroll={handleScroll}
          className="flex-1 overflow-y-auto"
        >
          {isLoading ? (
            <div className="flex h-full items-center justify-center">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : error ? (
            <div className="flex h-full items-center justify-center">
              <div className="text-center">
                <p className="text-sm font-medium text-destructive">
                  Failed to load conversation
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {error instanceof Error ? error.message : "Unknown error"}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => refetch()}
                  className="mt-3"
                >
                  Retry
                </Button>
              </div>
            </div>
          ) : runs.length === 0 ? (
            <div className="flex h-full items-center justify-center">
              <div className="text-center">
                <svg
                  className="mx-auto h-12 w-12 text-muted-foreground/50"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={1.5}
                  viewBox="0 0 24 24"
                  aria-label="Chat icon"
                >
                  <title>Chat</title>
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    d="M7.5 8.25h9m-9 3H12m-9.75 1.51c0 1.6 1.123 2.994 2.707 3.227 1.129.166 2.27.293 3.423.379.35.026.67.21.865.501L12 21l2.755-4.133a1.14 1.14 0 01.865-.501 48.172 48.172 0 003.423-.379c1.584-.233 2.707-1.626 2.707-3.228V6.741c0-1.602-1.123-2.995-2.707-3.228A48.394 48.394 0 0012 3c-2.392 0-4.744.175-7.043.513C3.373 3.746 2.25 5.14 2.25 6.741v6.018z"
                  />
                </svg>
                <p className="mt-4 text-sm font-medium text-muted-foreground">
                  No messages yet
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  Start a conversation by sending a message below
                </p>
              </div>
            </div>
          ) : (
            <div className="mx-auto max-w-4xl space-y-4 px-4 py-6">
              {conversationItems.map((item) => (
                <ConversationItemRow
                  key={item.id}
                  item={item}
                  onSelectCliJob={handleSelectCliJob}
                  onCopyUserMessage={handleCopyUserMessage}
                  onBranchFromRun={handleBranchFromRun}
                  onEditFromRun={handleStartEditFromRun}
                  editingUserMessage={
                    editingUserMessage === undefined
                      ? undefined
                      : {
                          runId: editingUserMessage.runId,
                          text: editingUserMessage.draft,
                          saving: branchSession.isPending || startRun.isPending,
                          onTextChange: (draft) =>
                            setEditingUserMessage((current) =>
                              current === undefined
                                ? current
                                : { ...current, draft },
                            ),
                          onSave: handleSaveEdit,
                          onCancel: handleCancelEdit,
                        }
                  }
                  onNavigateUserMessageVersion={
                    handleNavigateUserMessageVersion
                  }
                />
              ))}
              {currentRun === undefined ? null : (
                <ActiveRunStatusOutput
                  run={currentRun}
                  retryEvent={latestRetryEvent}
                />
              )}
            </div>
          )}
        </div>

        <SessionRightRail
          sections={railSections}
          contentKey={railContentKey}
          focusKey={`${selectedCliJobId ?? ""}:${railFocusSequence}`}
        />
      </div>

      <div className="border-t border-border p-4">
        <div className="mx-auto flex max-w-4xl flex-col gap-2">
          {actionError === undefined ? null : (
            <div className="rounded-sm border border-destructive/30 bg-destructive-soft px-3 py-2 text-sm text-destructive">
              {actionError}
            </div>
          )}
          {queuedMessageRuns.length > 0 ? (
            <QueuedMessagesBar
              runs={queuedMessageRuns}
              deliveringRunId={
                deliverQueuedRun.isPending
                  ? deliverQueuedRun.variables
                  : undefined
              }
              onDeliver={handleDeliverQueuedRun}
              deletingRunId={
                cancelRun.isPending ? cancelRun.variables : undefined
              }
              onDelete={handleDeleteQueuedRun}
            />
          ) : null}
          {pendingAsks.length > 0 ? (
            <div className="space-y-2">
              {pendingAsks.map((ask) => (
                <PendingAskCard
                  key={`${ask.runId}:${ask.ask.id}`}
                  ask={ask.ask}
                  submitting={
                    answerAsk.isPending &&
                    answerAsk.variables?.runId === ask.runId &&
                    answerAsk.variables?.askId === ask.ask.id
                  }
                  onSubmit={(answer) =>
                    answerAsk.mutate({
                      runId: ask.runId,
                      askId: ask.ask.id,
                      answer,
                    })
                  }
                />
              ))}
            </div>
          ) : null}
          <PromptInput
            onSubmit={handleSendMessage}
            onCancel={handleCancelRun}
            disabled={
              startRun.isPending ||
              branchSession.isPending ||
              cancelRun.isPending ||
              deliverQueuedRun.isPending
            }
            cancelling={cancelRun.isPending}
            running={currentRun !== undefined}
          />
        </div>
      </div>
    </div>
  );
}

function scrollToConversationBottom(scrollContainer: HTMLDivElement | null) {
  if (scrollContainer === null) {
    return;
  }

  scrollContainer.scrollTop = scrollContainer.scrollHeight;
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : fallback;
}
