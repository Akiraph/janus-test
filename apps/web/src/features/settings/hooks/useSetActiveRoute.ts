import type { SetActiveModelGatewayRouteInput } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { setActiveModelGatewayRoute } from "../../../lib/api-client";

/**
 * Mutation hook for setting the active model gateway route.
 * On success, invalidates the gateway status to reflect the new active route.
 */
export function useSetActiveRoute() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: SetActiveModelGatewayRouteInput) =>
      setActiveModelGatewayRoute(request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-gateway-status"] });
    },
  });
}
