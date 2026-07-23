import { useQuery } from "@tanstack/react-query";
import { listModelProviders } from "../../../lib/api-client";

/**
 * Query hook for fetching model providers list.
 * Providers include configuration for API routes, credentials, and model mappings.
 */
export function useModelProviders() {
  return useQuery({
    queryKey: ["model-providers"],
    queryFn: listModelProviders,
    staleTime: 30_000, // 30 seconds
  });
}
