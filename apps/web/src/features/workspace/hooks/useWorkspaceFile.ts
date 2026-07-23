import type { WorkspaceFileResponse } from "@janus/shared";
import { useQuery } from "@tanstack/react-query";
import { getWorkspaceFile } from "../../../lib/api-client";
import { workspaceKeys } from "../data/query-keys";

export function useWorkspaceFile(
  projectId: string,
  path: string,
  enabled = true,
) {
  return useQuery<WorkspaceFileResponse>({
    queryKey: workspaceKeys.workspaceFile(projectId, path),
    queryFn: () => getWorkspaceFile(projectId, path),
    enabled: enabled && path.length > 0,
    staleTime: 30_000,
  });
}
