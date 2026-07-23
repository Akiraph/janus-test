import { useMutation, useQueryClient } from "@tanstack/react-query";
import { deleteModelProvider } from "../../../lib/api-client";

export function useDeleteProvider() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (providerId: string) => deleteModelProvider(providerId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-providers"] });
      queryClient.invalidateQueries({ queryKey: ["model-gateway-status"] });
    },
  });
}
