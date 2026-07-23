import type { ListProjectThreadsResponse } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { renameSession } from "../../../lib/api-client";
import { broadcastProjectThreadsChanged } from "../data/project-thread-broadcast";
import { workspaceKeys } from "../data/query-keys";
import { renameThread } from "../data/thread-cache";

export function useRenameSession(projectId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      sessionId,
      title,
    }: {
      readonly sessionId: string;
      readonly title: string;
    }) => renameSession(sessionId, { title }),
    onSuccess: (data, variables) => {
      const queryKey = workspaceKeys.projectThreads(projectId);
      queryClient.setQueryData<ListProjectThreadsResponse>(
        queryKey,
        (current) =>
          renameThread(
            current,
            data.session.id,
            data.session.title ?? variables.title,
          ),
      );
      void queryClient.invalidateQueries({ queryKey });
      broadcastProjectThreadsChanged(projectId);
    },
  });
}
