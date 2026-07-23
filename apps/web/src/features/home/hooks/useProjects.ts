import { useQuery } from "@tanstack/react-query";
import { listProjects } from "../../../lib/api-client";

/**
 * Query hook for fetching the list of projects.
 * Automatically refetches on window focus and when stale.
 */
export function useProjects() {
  return useQuery({
    queryKey: ["projects"],
    queryFn: listProjects,
    staleTime: 30_000, // 30 seconds
    refetchOnWindowFocus: true,
  });
}
