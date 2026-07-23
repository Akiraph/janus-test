import type { WriteWorkspaceFileResponse } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { writeWorkspaceFile } from "../../../lib/api-client";
import { invalidateWorkspaceContentQueries } from "../data/query-invalidation";
import { workspaceKeys } from "../data/query-keys";

export interface WriteWorkspaceFileVariables {
  readonly path: string;
  readonly content: string;
}

/**
 * Persist edits to a workspace file. On success the returned file is written
 * straight into the matching file query cache so the editor's saved baseline
 * updates and the dirty marker clears without a refetch.
 */
export function useWriteWorkspaceFile(projectId: string) {
  const queryClient = useQueryClient();

  return useMutation<
    WriteWorkspaceFileResponse,
    Error,
    WriteWorkspaceFileVariables
  >({
    mutationFn: ({ path, content }) =>
      writeWorkspaceFile(projectId, path, content),
    onSuccess: (data, variables) => {
      queryClient.setQueryData<WriteWorkspaceFileResponse>(
        workspaceKeys.workspaceFile(projectId, variables.path),
        data,
      );
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}
