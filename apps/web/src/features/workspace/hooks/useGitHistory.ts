import { useQuery } from "@tanstack/react-query";
import { listGitHistory } from "../../../lib/api-client";

export function useGitHistory(projectId: string, limit = 20) {
  return useQuery({
    queryKey: ["git-history", projectId, limit],
    queryFn: () => listGitHistory(projectId, limit),
    staleTime: 60_000, // 1 minute
  });
}
