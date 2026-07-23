import { useSessionCheckpointsQuery } from "../data/queries";

export function useSessionCheckpoints(sessionId: string) {
  return useSessionCheckpointsQuery(sessionId);
}
