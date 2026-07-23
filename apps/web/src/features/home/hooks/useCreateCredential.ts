import type { ConfigureCredentialRequest } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { configureCredential } from "../../../lib/api-client";

/**
 * Mutation hook for creating a new credential.
 * On success, invalidates the credentials list to trigger a refetch.
 */
export function useCreateCredential() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: ConfigureCredentialRequest) =>
      configureCredential(request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["credentials"] });
    },
  });
}
