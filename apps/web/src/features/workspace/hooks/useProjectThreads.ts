import { useProjectThreadsQuery } from "../data/queries";

export function useProjectThreads(projectId: string) {
  return useProjectThreadsQuery(projectId);
}
