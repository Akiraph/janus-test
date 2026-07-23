import type { UpsertModelProviderInput } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { upsertModelProvider } from "../../../lib/api-client";

/**
 * Mutation hook for creating or updating a model provider.
 * On success, invalidates the providers list to trigger a refetch.
 */
export function useUpsertProvider() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: UpsertModelProviderInput) =>
      upsertModelProvider(request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-providers"] });
      queryClient.invalidateQueries({ queryKey: ["model-gateway-status"] });
    },
  });
}
