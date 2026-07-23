import { useQuery } from "@tanstack/react-query";
import { listCredentials } from "../../../lib/api-client";

/**
 * Query hook for fetching GitHub credentials list.
 * Used in the Connect Repository dialog to select a credential.
 */
export function useCredentials() {
  return useQuery({
    queryKey: ["credentials"],
    queryFn: listCredentials,
    staleTime: 60_000, // 1 minute
  });
}
