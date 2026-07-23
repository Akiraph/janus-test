import type {
  CreateSessionInput,
  ListProjectThreadsResponse,
} from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { createSession } from "../../../lib/api-client";
import { broadcastProjectThreadsChanged } from "../data/project-thread-broadcast";
import { workspaceKeys } from "../data/query-keys";
import { threadFromSession, upsertThread } from "../data/thread-cache";

export function useCreateSession() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: CreateSessionInput) => createSession(request),
    onSuccess: (data) => {
      const queryKey = workspaceKeys.projectThreads(data.session.projectId);
      queryClient.setQueryData<ListProjectThreadsResponse>(
        queryKey,
        (current) => upsertThread(current, threadFromSession(data.session)),
      );
      void queryClient.invalidateQueries({ queryKey });
      broadcastProjectThreadsChanged(data.session.projectId);
    },
  });
}
