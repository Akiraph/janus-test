import type {
  DeleteWorkspacePathResponse,
  RenameWorkspacePathResponse,
} from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  deleteWorkspacePath,
  renameWorkspacePath,
} from "../../../lib/api-client";
import { invalidateWorkspaceContentQueries } from "../data/query-invalidation";

export interface RenameWorkspacePathVariables {
  readonly fromPath: string;
  readonly toPath: string;
}

export interface DeleteWorkspacePathVariables {
  readonly path: string;
}

export function useRenameWorkspacePath(projectId: string) {
  const queryClient = useQueryClient();

  return useMutation<
    RenameWorkspacePathResponse,
    Error,
    RenameWorkspacePathVariables
  >({
    mutationFn: (request) => renameWorkspacePath(projectId, request),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}

export function useDeleteWorkspacePath(projectId: string) {
  const queryClient = useQueryClient();

  return useMutation<
    DeleteWorkspacePathResponse,
    Error,
    DeleteWorkspacePathVariables
  >({
    mutationFn: (request) => deleteWorkspacePath(projectId, request),
    onSuccess: () => {
      invalidateWorkspaceContentQueries(queryClient, projectId);
    },
  });
}
