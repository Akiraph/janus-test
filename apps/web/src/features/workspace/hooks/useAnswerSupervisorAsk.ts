import { useMutation, useQueryClient } from "@tanstack/react-query";
import { answerSupervisorAsk } from "../../../lib/api-client";
import { workspaceKeys } from "../data/query-keys";

interface AnswerSupervisorAskParams {
  readonly runId: string;
  readonly askId: string;
  readonly answer: string;
}

export function useAnswerSupervisorAsk(sessionId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ runId, askId, answer }: AnswerSupervisorAskParams) =>
      answerSupervisorAsk(runId, askId, { answer }),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(sessionId),
      });
    },
  });
}
