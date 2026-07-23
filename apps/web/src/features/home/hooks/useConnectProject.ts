import type { ConnectProjectInput } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { connectProject } from "../../../lib/api-client";

/**
 * Mutation hook for connecting a new repository.
 * On success, invalidates the projects list to trigger a refetch.
 */
export function useConnectProject() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (input: ConnectProjectInput) => connectProject(input),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["projects"] });
    },
  });
}
