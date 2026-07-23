import { useQuery } from "@tanstack/react-query";
import { getSessionRuntime } from "../../lib/api-client";

export function sessionRuntimeQueryKey(sessionId: string | undefined) {
  return ["session-runtime", sessionId] as const;
}

export function useSessionRuntime(sessionId: string | undefined) {
  return useQuery({
    queryKey: sessionRuntimeQueryKey(sessionId),
    queryFn: () => getSessionRuntime(requireSessionId(sessionId)),
    enabled: sessionId !== undefined,
    refetchInterval: sessionId === undefined ? false : 3000,
    retry: false,
  });
}

function requireSessionId(sessionId: string | undefined): string {
  if (sessionId === undefined) {
    throw new Error("Session id is required.");
  }

  return sessionId;
}
