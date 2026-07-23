import { useSessionActivityQuery } from "../data/queries";

export function useSessionActivity(
  sessionId: string,
  projectId: string,
  live = false,
) {
  return useSessionActivityQuery(sessionId, projectId, live);
}
