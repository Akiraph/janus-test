import type {
  ListSessionApplyRecordsResponse,
  SessionApplyRecord,
  StartSessionApplyRequest,
} from "@janus/shared";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef } from "react";
import {
  applySessionApplyReview,
  listSessionApplyRecords,
  refreshSessionApplyReview,
  startSessionApply,
  subscribeSupervisorRunStream,
} from "../../../lib/api-client";
import { invalidateWorkspaceContentQueries } from "../data/query-invalidation";
import { workspaceKeys } from "../data/query-keys";

const APPLY_REVIEW_LIVE_REFRESH_MS = 2500;

export function useSessionApplyRecords(sessionId: string | undefined) {
  const queryClient = useQueryClient();
  const refreshedResolutionRunsRef = useRef<Set<string>>(new Set());
  const query = useQuery<ListSessionApplyRecordsResponse>({
    queryKey: workspaceKeys.sessionApplyRecords(sessionId ?? ""),
    queryFn: () => {
      if (sessionId === undefined) {
        throw new Error("Session id is required.");
      }

      return listSessionApplyRecords(sessionId);
    },
    enabled: sessionId !== undefined,
  });
  const resolutionWatches = useMemo(
    () => resolutionApplyWatches(query.data?.applies ?? []),
    [query.data?.applies],
  );
  const liveRefreshTarget = useMemo(
    () => findLiveApplyReviewTarget(query.data?.applies ?? []),
    [query.data?.applies],
  );
  const liveRefreshTargetId = liveRefreshTarget?.id;

  useEffect(() => {
    if (sessionId === undefined || resolutionWatches.length === 0) {
      return;
    }

    const unsubscribes = resolutionWatches.map((watch) =>
      subscribeSupervisorRunStream(
        watch.resolutionSessionId,
        (event) => {
          if (
            event.type !== "run_updated" ||
            event.run.id !== watch.resolutionRunId ||
            !isTerminalRunStatus(event.run.status)
          ) {
            return;
          }

          const marker = [
            watch.applyId,
            event.run.id,
            event.run.status,
            event.run.completedAt ?? "",
          ].join(":");
          if (refreshedResolutionRunsRef.current.has(marker)) {
            return;
          }

          refreshedResolutionRunsRef.current.add(marker);
          void refreshSessionApplyReview(sessionId, watch.applyId).finally(
            () => {
              void queryClient.invalidateQueries({
                queryKey: workspaceKeys.sessionApplyRecords(sessionId),
              });
            },
          );
        },
        (error) => {
          console.error("Apply resolution run stream failed:", error);
        },
      ),
    );

    return () => {
      unsubscribes.forEach((unsubscribe) => {
        unsubscribe();
      });
    };
  }, [queryClient, resolutionWatches, sessionId]);

  useEffect(() => {
    if (sessionId === undefined || liveRefreshTargetId === undefined) {
      return;
    }

    let disposed = false;
    let inFlight = false;
    const refresh = () => {
      if (inFlight) {
        return;
      }

      inFlight = true;
      void refreshSessionApplyReview(sessionId, liveRefreshTargetId)
        .then((response) => {
          if (disposed) {
            return;
          }

          queryClient.setQueryData<ListSessionApplyRecordsResponse>(
            workspaceKeys.sessionApplyRecords(sessionId),
            (current) => upsertApplyRecord(current, response.apply),
          );
        })
        .catch((error) => {
          console.error("Apply review live refresh failed:", error);
        })
        .finally(() => {
          inFlight = false;
        });
    };

    refresh();
    const intervalId = window.setInterval(
      refresh,
      APPLY_REVIEW_LIVE_REFRESH_MS,
    );

    return () => {
      disposed = true;
      window.clearInterval(intervalId);
    };
  }, [liveRefreshTargetId, queryClient, sessionId]);

  return query;
}

export function findLiveApplyReviewTarget(
  applies: readonly SessionApplyRecord[],
): SessionApplyRecord | undefined {
  return applies.find((apply) => shouldLiveRefreshApplyReview(apply));
}

export function upsertApplyRecord(
  current: ListSessionApplyRecordsResponse | undefined,
  apply: SessionApplyRecord,
): ListSessionApplyRecordsResponse {
  const applies =
    current?.applies.filter((candidate) => candidate.id !== apply.id) ?? [];

  return {
    applies: [apply, ...applies].sort(compareApplyRecords),
  };
}

function shouldLiveRefreshApplyReview(apply: SessionApplyRecord): boolean {
  return (
    apply.status === "integrating" ||
    apply.status === "conflicted" ||
    apply.status === "resolving"
  );
}

function compareApplyRecords(
  left: SessionApplyRecord,
  right: SessionApplyRecord,
): number {
  if (left.createdAt !== right.createdAt) {
    return right.createdAt.localeCompare(left.createdAt);
  }

  return right.id.localeCompare(left.id);
}

function resolutionApplyWatches(
  applies: readonly SessionApplyRecord[],
): readonly {
  readonly applyId: string;
  readonly resolutionSessionId: string;
  readonly resolutionRunId: string;
}[] {
  return applies
    .map((apply) => {
      const resolutionSessionId = apply.conflictResolution.sessionId;
      const resolutionRunId = apply.conflictResolution.runId;

      if (
        resolutionSessionId === undefined ||
        resolutionRunId === undefined ||
        apply.status !== "resolving"
      ) {
        return undefined;
      }

      return {
        applyId: apply.id,
        resolutionSessionId,
        resolutionRunId,
      };
    })
    .filter(
      (
        watch,
      ): watch is {
        readonly applyId: string;
        readonly resolutionSessionId: string;
        readonly resolutionRunId: string;
      } => watch !== undefined,
    );
}

function isTerminalRunStatus(status: string): boolean {
  return status === "completed" || status === "failed" || status === "canceled";
}

export function useStartSessionApply(sessionId: string, projectId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: StartSessionApplyRequest = {}) =>
      startSessionApply(sessionId, request),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionApplyRecords(sessionId),
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.projectThreads(projectId),
      });
      const resolutionSessionId = response.apply.conflictResolution.sessionId;
      if (resolutionSessionId !== undefined) {
        void queryClient.invalidateQueries({
          queryKey: workspaceKeys.sessionRuns(resolutionSessionId),
        });
      }
    },
  });
}

export function useRefreshSessionApplyReview(sessionId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (applyId: string) =>
      refreshSessionApplyReview(sessionId, applyId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionApplyRecords(sessionId),
      });
    },
  });
}

export function useApplySessionApplyReview(
  sessionId: string,
  projectId: string,
) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (applyId: string) =>
      applySessionApplyReview(sessionId, applyId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionApplyRecords(sessionId),
      });
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}
