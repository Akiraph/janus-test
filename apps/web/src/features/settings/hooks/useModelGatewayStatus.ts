import { useQuery } from "@tanstack/react-query";
import { getModelGatewayStatus } from "../../../lib/api-client";

/**
 * Query hook for fetching model gateway status.
 * Includes active route and health status for all providers.
 */
export function useModelGatewayStatus() {
  return useQuery({
    queryKey: ["model-gateway-status"],
    queryFn: getModelGatewayStatus,
    staleTime: 30_000, // 30 seconds
    refetchInterval: 60_000, // Auto-refresh every minute
  });
}
