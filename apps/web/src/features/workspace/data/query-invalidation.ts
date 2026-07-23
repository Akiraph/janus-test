import type { QueryClient } from "@tanstack/react-query";
import { workspaceKeys } from "./query-keys";

export function invalidateWorkspaceContentQueries(
  queryClient: QueryClient,
  projectId: string,
): void {
  void queryClient.invalidateQueries({
    queryKey: workspaceKeys.workspaceTreeScope(projectId),
  });
  void queryClient.invalidateQueries({
    queryKey: workspaceKeys.workspaceFileScope(projectId),
  });
  void queryClient.invalidateQueries({
    queryKey: workspaceKeys.gitStatus(projectId),
  });
  void queryClient.invalidateQueries({
    queryKey: workspaceKeys.gitHistory(projectId),
  });
}

export function invalidateSessionDiffQueries(
  queryClient: QueryClient,
  sessionId: string,
): void {
  void queryClient.invalidateQueries({
    queryKey: workspaceKeys.sessionDiff(sessionId),
  });
}
