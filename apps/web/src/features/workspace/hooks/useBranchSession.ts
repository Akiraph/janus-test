import type {
  BranchSessionRequest,
  ListProjectThreadsResponse,
} from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { branchSession } from "../../../lib/api-client";
import { broadcastProjectThreadsChanged } from "../data/project-thread-broadcast";
import { workspaceKeys } from "../data/query-keys";
import { threadFromSession, upsertThread } from "../data/thread-cache";

interface BranchSessionParams extends BranchSessionRequest {
  readonly sessionId: string;
  readonly projectId: string;
}

export function useBranchSession() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      sessionId,
      projectId: _projectId,
      ...request
    }: BranchSessionParams) => branchSession(sessionId, request),
    onSuccess: (response, variables) => {
      const queryKey = workspaceKeys.projectThreads(variables.projectId);
      queryClient.setQueryData<ListProjectThreadsResponse>(
        queryKey,
        (current) =>
          upsertThread(
            current,
            threadFromSession(response.session, {
              runCount: response.copiedRunCount,
              status: response.copiedRunCount > 0 ? "completed" : "idle",
            }),
          ),
      );
      void queryClient.invalidateQueries({
        queryKey,
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(response.session.id),
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionCheckpoints(response.session.id),
      });
      broadcastProjectThreadsChanged(variables.projectId);
    },
  });
}
