import { useQuery } from "@tanstack/react-query";
import { getAuthStatus } from "../../../lib/api-client";

export function useAuthStatus() {
  return useQuery({
    queryKey: ["auth-status"],
    queryFn: getAuthStatus,
    retry: false,
    staleTime: 5_000,
  });
}
