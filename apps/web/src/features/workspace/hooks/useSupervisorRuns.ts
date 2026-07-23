import { useSessionRunsQuery } from "../data/queries";

export function useSupervisorRuns(sessionId: string) {
  return useSessionRunsQuery(sessionId);
}
