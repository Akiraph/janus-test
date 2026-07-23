import type {
  ActivityEvent,
  CreateSessionInput,
  GitHistoryResponse,
  GitStatusResponse,
  ListProjectThreadsResponse,
  ListSessionCheckpointsResponse,
  ListSupervisorRunsResponse,
  ProjectThreadsLiveEvent,
  SessionActivityResponse,
  SessionDiff,
  SessionRuntimeResponse,
  SupervisorRunLiveEvent,
  SupervisorRunRecord,
  WorkspaceTreeResponse,
} from "@janus/shared";
import {
  type QueryClient,
  type UseQueryResult,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import {
  approveRuntimeApprovalRequest,
  commitGitChanges,
  createSession,
  denyRuntimeApprovalRequest,
  dispatchInstruction,
  getGitStatus,
  getSessionActivity,
  getSessionDiff,
  getSessionRuntime,
  getWorkspaceTree,
  listGitHistory,
  listProjectThreads,
  listSessionCheckpoints,
  listSupervisorRuns,
  stageAllGitFiles,
  stageGitFile,
  subscribeProjectThreadsStream,
  subscribeSessionActivityStream,
  subscribeSupervisorRunStream,
  unstageGitFile,
} from "../../../lib/api-client";
import {
  broadcastProjectThreadsChanged,
  subscribeProjectThreadsBroadcast,
} from "./project-thread-broadcast";
import {
  invalidateSessionDiffQueries,
  invalidateWorkspaceContentQueries,
} from "./query-invalidation";
import { workspaceKeys } from "./query-keys";
import { upsertRun } from "./supervisor-run-cache";
import {
  removeThread,
  threadFromSession,
  updateThreadFromRun,
  upsertThread,
} from "./thread-cache";

const LIVE_REFETCH_MS = 4000;

export function useProjectThreadsQuery(
  projectId: string | undefined,
): UseQueryResult<ListProjectThreadsResponse> {
  const queryClient = useQueryClient();
  const seenThreadRefreshesRef = useRef<Set<string>>(new Set());
  const query = useQuery({
    queryKey: workspaceKeys.projectThreads(projectId ?? ""),
    queryFn: () =>
      projectId === undefined
        ? Promise.resolve({ threads: [] })
        : listProjectThreads(projectId),
    enabled: projectId !== undefined,
    refetchInterval: LIVE_REFETCH_MS,
  });

  useEffect(() => {
    if (projectId === undefined) {
      return;
    }

    return subscribeProjectThreadsStream(
      projectId,
      (event) => {
        const shouldRefreshThreads = applyProjectThreadsEvent(
          queryClient,
          seenThreadRefreshesRef.current,
          event,
        );

        if (event.type === "threads_changed" && shouldRefreshThreads) {
          broadcastProjectThreadsChanged(event.projectId);
        }
      },
      (error) => {
        console.error("Project threads stream failed:", error);
      },
    );
  }, [projectId, queryClient]);

  useEffect(() => {
    if (projectId === undefined) {
      return;
    }

    return subscribeProjectThreadsBroadcast(projectId, () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.projectThreads(projectId),
      });
    });
  }, [projectId, queryClient]);

  return query;
}

function applyProjectThreadsEvent(
  queryClient: QueryClient,
  seenThreadRefreshes: Set<string>,
  event: ProjectThreadsLiveEvent,
): boolean {
  if (event.type === "threads_snapshot") {
    queryClient.setQueryData<ListProjectThreadsResponse>(
      workspaceKeys.projectThreads(event.projectId),
      { threads: event.threads },
    );
    return false;
  }

  const run = event.run;
  let shouldRefreshThreads = true;

  if (run !== undefined) {
    queryClient.setQueryData<ListProjectThreadsResponse>(
      workspaceKeys.projectThreads(event.projectId),
      (current) => updateThreadFromRun(current, run),
    );
    queryClient.setQueryData<ListSupervisorRunsResponse>(
      workspaceKeys.sessionRuns(run.sessionId),
      (current) => upsertRun(current, run),
    );
    void queryClient.invalidateQueries({
      queryKey: workspaceKeys.sessionRuns(run.sessionId),
    });

    const marker = runThreadRefreshMarker(run);
    shouldRefreshThreads = !seenThreadRefreshes.has(marker);

    if (shouldRefreshThreads) {
      seenThreadRefreshes.add(marker);
    }
  }

  const deletedSessionId = event.sessionId;
  if (event.reason === "session_deleted" && deletedSessionId !== undefined) {
    queryClient.setQueryData<ListProjectThreadsResponse>(
      workspaceKeys.projectThreads(event.projectId),
      (current) => removeThread(current, deletedSessionId),
    );
  }

  if (shouldRefreshThreads) {
    void queryClient.invalidateQueries({
      queryKey: workspaceKeys.projectThreads(event.projectId),
    });
  }

  return shouldRefreshThreads;
}

export function useWorkspaceTreeQuery(
  projectId: string,
  path: string,
  enabled = true,
): UseQueryResult<WorkspaceTreeResponse> {
  return useQuery({
    queryKey: workspaceKeys.workspaceTree(projectId, path),
    queryFn: () => getWorkspaceTree(projectId, path),
    enabled,
  });
}

export function useGitHistoryQuery(
  projectId: string | undefined,
): UseQueryResult<GitHistoryResponse> {
  return useQuery({
    queryKey: workspaceKeys.gitHistory(projectId ?? ""),
    queryFn: () =>
      projectId === undefined
        ? Promise.resolve({ branches: [], commits: [] })
        : listGitHistory(projectId),
    enabled: projectId !== undefined,
  });
}

export function useGitStatusQuery(
  projectId: string | undefined,
): UseQueryResult<GitStatusResponse> {
  return useQuery({
    queryKey: workspaceKeys.gitStatus(projectId ?? ""),
    queryFn: () =>
      projectId === undefined
        ? missingQueryParam("projectId")
        : getGitStatus(projectId),
    enabled: projectId !== undefined,
    refetchInterval: LIVE_REFETCH_MS,
  });
}

export function useSessionRunsQuery(
  sessionId: string | undefined,
): UseQueryResult<ListSupervisorRunsResponse> {
  const queryClient = useQueryClient();
  const seenWorkspaceRefreshesRef = useRef<Set<string>>(new Set());
  const seenThreadRefreshesRef = useRef<Set<string>>(new Set());
  const query = useQuery({
    queryKey: workspaceKeys.sessionRuns(sessionId ?? ""),
    queryFn: () =>
      sessionId === undefined
        ? Promise.resolve({ runs: [] })
        : listSupervisorRuns(sessionId),
    enabled: sessionId !== undefined,
    refetchOnWindowFocus: true,
    refetchInterval: LIVE_REFETCH_MS,
  });

  useEffect(() => {
    const hasRuns = (query.data?.runs.length ?? 0) > 0;

    if (sessionId === undefined || !hasRuns) {
      return;
    }

    return subscribeSupervisorRunStream(
      sessionId,
      (event) => {
        const updatedRuns =
          queryClient.setQueryData<ListSupervisorRunsResponse>(
            workspaceKeys.sessionRuns(sessionId),
            (current) => applySupervisorRunLiveEvent(current, event),
          );
        if (event.type !== "run_updated") {
          const updatedRun = updatedRuns?.runs.find(
            (run) => run.id === event.runId,
          );

          if (updatedRun !== undefined) {
            queryClient.setQueryData<ListProjectThreadsResponse>(
              workspaceKeys.projectThreads(updatedRun.projectId),
              (current) => updateThreadFromRun(current, updatedRun),
            );
          }

          return;
        }

        queryClient.setQueryData<ListProjectThreadsResponse>(
          workspaceKeys.projectThreads(event.run.projectId),
          (current) => updateThreadFromRun(current, event.run),
        );
        const marker = runThreadRefreshMarker(event.run);
        if (!seenThreadRefreshesRef.current.has(marker)) {
          seenThreadRefreshesRef.current.add(marker);
          void queryClient.invalidateQueries({
            queryKey: workspaceKeys.projectThreads(event.run.projectId),
          });
        }
      },
      (error) => {
        console.error("Supervisor run stream failed:", error);
      },
    );
  }, [query.data?.runs.length, queryClient, sessionId]);

  useEffect(() => {
    if (sessionId === undefined) {
      return;
    }

    const runs = query.data?.runs ?? [];

    for (const run of runs) {
      const marker = runWorkspaceRefreshMarker(run);

      if (
        marker === undefined ||
        seenWorkspaceRefreshesRef.current.has(marker)
      ) {
        continue;
      }

      seenWorkspaceRefreshesRef.current.add(marker);
      invalidateWorkspaceContentQueries(queryClient, run.projectId);
      invalidateSessionDiffQueries(queryClient, sessionId);
    }
  }, [query.data?.runs, queryClient, sessionId]);

  return query;
}

function applySupervisorRunLiveEvent(
  current: ListSupervisorRunsResponse | undefined,
  event: SupervisorRunLiveEvent,
): ListSupervisorRunsResponse | undefined {
  switch (event.type) {
    case "run_updated":
      return upsertRun(current, event.run);
    case "transcript_entry_updated":
      return upsertTranscriptEntry(current, event);
  }
}

function runThreadRefreshMarker(run: SupervisorRunRecord): string {
  return [
    run.id,
    run.status,
    run.deliveryRequestedAt ?? "",
    run.deliveredAt ?? "",
    run.deliveredToRunId ?? "",
    run.completedAt ?? "",
    run.lastError ?? "",
  ].join(":");
}

function upsertTranscriptEntry(
  current: ListSupervisorRunsResponse | undefined,
  event: Extract<SupervisorRunLiveEvent, { type: "transcript_entry_updated" }>,
): ListSupervisorRunsResponse | undefined {
  if (current === undefined) {
    return current;
  }

  let updated = false;
  const runs = current.runs.map((run) => {
    if (run.id !== event.runId) {
      return run;
    }

    updated = true;
    const entryIndex = run.transcript.findIndex(
      (entry) => entry.id === event.entry.id,
    );
    const transcript =
      entryIndex === -1
        ? [...run.transcript, event.entry]
        : run.transcript.map((entry, index) =>
            index === entryIndex ? event.entry : entry,
          );

    return {
      ...run,
      transcript,
      updatedAt: event.updatedAt,
    };
  });

  return updated ? { runs } : current;
}

export function useSessionCheckpointsQuery(
  sessionId: string | undefined,
): UseQueryResult<ListSessionCheckpointsResponse> {
  return useQuery({
    queryKey: workspaceKeys.sessionCheckpoints(sessionId ?? ""),
    queryFn: () =>
      sessionId === undefined
        ? Promise.resolve({ checkpoints: [] })
        : listSessionCheckpoints(sessionId),
    enabled: sessionId !== undefined,
  });
}

function runWorkspaceRefreshMarker(
  run: SupervisorRunRecord,
): string | undefined {
  if (run.diff === undefined) {
    const workspaceWriteCount = successfulWorkspaceWriteCount(run);

    if (workspaceWriteCount === 0) {
      return undefined;
    }

    return `${run.id}:workspace-write:${workspaceWriteCount}:${run.status}`;
  }

  return `${run.id}:${run.diff.updatedAt}`;
}

function successfulWorkspaceWriteCount(run: SupervisorRunRecord): number {
  return run.transcript.filter(
    (entry) =>
      entry.kind === "tool" &&
      entry.status === "completed" &&
      (entry.name === "write_file" || entry.name === "edit_file"),
  ).length;
}

export function useSessionActivityQuery(
  sessionId: string | undefined,
  projectId?: string | undefined,
  live = false,
): UseQueryResult<SessionActivityResponse> {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: workspaceKeys.sessionActivity(sessionId ?? ""),
    queryFn: () =>
      sessionId === undefined
        ? Promise.resolve({ events: [] })
        : getSessionActivity(sessionId),
    enabled: sessionId !== undefined,
    refetchOnWindowFocus: true,
    refetchInterval: live ? LIVE_REFETCH_MS : false,
  });

  useEffect(() => {
    if (sessionId === undefined) {
      return;
    }

    return subscribeSessionActivityStream(
      sessionId,
      (event) => {
        queryClient.setQueryData<SessionActivityResponse>(
          workspaceKeys.sessionActivity(sessionId),
          (current) => upsertActivityEvent(current, event),
        );
        handleActivityInvalidation(queryClient, event, projectId);
      },
      (error) => {
        console.error("Session activity stream failed:", error);
      },
    );
  }, [projectId, queryClient, sessionId]);

  return query;
}

function upsertActivityEvent(
  current: SessionActivityResponse | undefined,
  event: ActivityEvent,
): SessionActivityResponse {
  const events =
    current?.events.filter((candidate) => candidate.id !== event.id) ?? [];
  return {
    events: [...events, event].sort((left, right) =>
      left.sequence === right.sequence
        ? left.id.localeCompare(right.id)
        : left.sequence - right.sequence,
    ),
  };
}

function handleActivityInvalidation(
  queryClient: QueryClient,
  event: ActivityEvent,
  projectId: string | undefined,
): void {
  switch (event.type) {
    case "checkpoint_recorded":
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionCheckpoints(event.sessionId),
      });
      invalidateSessionDiffQueries(queryClient, event.sessionId);
      if (projectId !== undefined) {
        invalidateWorkspaceContentQueries(queryClient, projectId);
      }
      break;
    case "diff_recorded":
      invalidateSessionDiffQueries(queryClient, event.sessionId);
      if (projectId !== undefined) {
        invalidateWorkspaceContentQueries(queryClient, projectId);
      }
      break;
    case "sandbox_started":
    case "session_failed":
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuntime(event.sessionId),
      });
      if (projectId === undefined) {
        break;
      }
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.projectThreads(projectId),
      });
      break;
    case "session_created":
    case "session_renamed":
    case "session_completed":
    case "session_canceled":
      if (projectId !== undefined) {
        void queryClient.invalidateQueries({
          queryKey: workspaceKeys.projectThreads(projectId),
        });
      }
      break;
    case "cli_dispatched":
    case "cli_output":
    case "discussion_started":
    case "discussion_participant_started":
    case "discussion_participant_completed":
    case "discussion_round_completed":
    case "discussion_completed":
    case "discussion_partial":
    case "discussion_failed":
    case "discussion_cancelled":
    case "supervisor_model_retry":
    case "supervisor_model_recovered":
    case "supervisor_model_warning":
      break;
  }
}

export function useSessionRuntimeQuery(
  sessionId: string | undefined,
): UseQueryResult<SessionRuntimeResponse> {
  return useQuery({
    queryKey: workspaceKeys.sessionRuntime(sessionId ?? ""),
    queryFn: () =>
      sessionId === undefined
        ? missingQueryParam("sessionId")
        : getSessionRuntime(sessionId),
    enabled: sessionId !== undefined,
    refetchInterval: LIVE_REFETCH_MS,
  });
}

export function useSessionDiffQuery(
  sessionId: string | undefined,
): UseQueryResult<SessionDiff> {
  return useQuery({
    queryKey: workspaceKeys.sessionDiff(sessionId ?? ""),
    queryFn: () =>
      sessionId === undefined
        ? missingQueryParam("sessionId")
        : getSessionDiff(sessionId),
    enabled: sessionId !== undefined,
    retry: false,
  });
}

function missingQueryParam(name: string): Promise<never> {
  return Promise.reject(new Error(`${name} is required.`));
}

export function useDispatchInstructionMutation(sessionId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (instruction: string) =>
      dispatchInstruction(sessionId, { instruction }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionActivity(sessionId),
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(sessionId),
      });
      invalidateSessionDiffQueries(queryClient, sessionId);
      invalidateWorkspaceContentQueries(
        queryClient,
        response.session.projectId,
      );
    },
  });
}

export function useCreateSessionMutation(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateSessionInput) => createSession(input),
    onSuccess: (data) => {
      queryClient.setQueryData<ListProjectThreadsResponse>(
        workspaceKeys.projectThreads(projectId),
        (current) => upsertThread(current, threadFromSession(data.session)),
      );
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.projectThreads(projectId),
      });
      broadcastProjectThreadsChanged(projectId);
    },
  });
}

export function useStageGitFileMutation(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (path: string) => stageGitFile(projectId, path),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}

export function useUnstageGitFileMutation(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (path: string) => unstageGitFile(projectId, path),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}

export function useStageAllGitFilesMutation(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => stageAllGitFiles(projectId),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}

export function useCommitGitChangesMutation(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (message: string) => commitGitChanges(projectId, message),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}

export function useResolveApprovalMutation(sessionId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      approvalRequestId,
      decision,
    }: {
      approvalRequestId: string;
      decision: "approve" | "deny";
    }) =>
      decision === "approve"
        ? approveRuntimeApprovalRequest(sessionId, approvalRequestId)
        : denyRuntimeApprovalRequest(sessionId, approvalRequestId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuntime(sessionId),
      });
    },
  });
}
