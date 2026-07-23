import type { WorkspaceTreeResponse } from "@janus/shared";
import type { UseQueryOptions } from "@tanstack/react-query";
import { useQuery } from "@tanstack/react-query";
import { getWorkspaceTree } from "../../../lib/api-client";
import { workspaceKeys } from "../data/query-keys";

const WORKSPACE_TREE_REFETCH_MS = 3000;

export function useWorkspaceTree(
  projectId: string,
  path = "",
  options?: Omit<
    UseQueryOptions<WorkspaceTreeResponse>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery({
    queryKey: workspaceKeys.workspaceTree(projectId, path),
    queryFn: () => getWorkspaceTree(projectId, path),
    staleTime: 30_000, // 30 seconds
    refetchInterval: WORKSPACE_TREE_REFETCH_MS,
    refetchOnMount: "always",
    refetchOnWindowFocus: "always",
    ...options,
  });
}
