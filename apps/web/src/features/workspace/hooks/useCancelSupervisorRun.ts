import { useMutation, useQueryClient } from "@tanstack/react-query";
import { cancelSupervisorRun } from "../../../lib/api-client";
import { workspaceKeys } from "../data/query-keys";

export function useCancelSupervisorRun(sessionId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (runId: string) => cancelSupervisorRun(runId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(sessionId),
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionActivity(sessionId),
      });
    },
  });
}
