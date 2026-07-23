import type { TestModelProviderInput } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { testModelProvider } from "../../../lib/api-client";

export function useTestProvider() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: TestModelProviderInput) => testModelProvider(request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-gateway-status"] });
    },
  });
}
