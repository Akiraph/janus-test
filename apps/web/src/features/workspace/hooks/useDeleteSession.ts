import type { ListProjectThreadsResponse } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { deleteSession } from "../../../lib/api-client";
import { broadcastProjectThreadsChanged } from "../data/project-thread-broadcast";
import { workspaceKeys } from "../data/query-keys";
import { removeThread } from "../data/thread-cache";

export function useDeleteSession(projectId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (sessionId: string) => deleteSession(sessionId),
    onSuccess: (_data, sessionId) => {
      const queryKey = workspaceKeys.projectThreads(projectId);
      queryClient.setQueryData<ListProjectThreadsResponse>(
        queryKey,
        (current) => removeThread(current, sessionId),
      );
      void queryClient.invalidateQueries({
        queryKey,
      });
      broadcastProjectThreadsChanged(projectId);
    },
  });
}
