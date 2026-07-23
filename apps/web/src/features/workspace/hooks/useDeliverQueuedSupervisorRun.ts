import { useMutation, useQueryClient } from "@tanstack/react-query";
import { deliverQueuedSupervisorRun } from "../../../lib/api-client";
import { workspaceKeys } from "../data/query-keys";

export function useDeliverQueuedSupervisorRun(sessionId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (runId: string) => deliverQueuedSupervisorRun(runId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(sessionId),
      });
    },
  });
}
