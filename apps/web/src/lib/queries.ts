import { createQuery, useQueryClient } from "@tanstack/solid-query";
import {
  getBootstrap,
  getMe,
  getProject,
  getProviders,
  getQueuedTurns,
  getSession,
  getSessionContext,
  getSessionDiff,
  getSessionTimeline,
  getSystemInfo,
  getTurn,
  gitLog,
  gitStatus,
  listFileTree,
  listGithubCredentials,
  listProjects,
  listSessions,
  type SessionSummary,
} from "./api";
import { sessionTimelineRefetchInterval } from "./queryPolicy";

export function useBootstrap() {
  return createQuery(() => ({
    queryKey: ["bootstrap"],
    queryFn: getBootstrap,
  }));
}

export function useSystemInfo() {
  return createQuery(() => ({
    queryKey: ["system-info"],
    queryFn: getSystemInfo,
  }));
}

export function useMe() {
  return createQuery(() => ({ queryKey: ["me"], queryFn: getMe, retry: false }));
}

export function useProviders() {
  return createQuery(() => ({ queryKey: ["model-providers"], queryFn: getProviders }));
}

export function useProjects() {
  return createQuery(() => ({
    queryKey: ["projects"],
    queryFn: () => listProjects(),
  }));
}

export function useProject(id: () => string | undefined) {
  return createQuery(() => {
    const projectId = id();
    return {
      queryKey: ["project", projectId],
      queryFn: () => getProject(projectId as string),
      enabled: Boolean(projectId),
    };
  });
}

export function useGithubCredentials() {
  return createQuery(() => ({
    queryKey: ["github-credentials"],
    queryFn: listGithubCredentials,
  }));
}

export function useFileTree(projectId: () => string | undefined, path: () => string = () => "") {
  return createQuery(() => {
    const id = projectId();
    const treePath = path();
    return {
      queryKey: ["file-tree", id, treePath],
      queryFn: () => listFileTree(id as string, treePath || undefined),
      enabled: Boolean(id),
    };
  });
}

export function useGitStatus(projectId: () => string | undefined) {
  return createQuery(() => {
    const id = projectId();
    return {
      queryKey: ["git-status", id],
      queryFn: () => gitStatus(id as string),
      enabled: Boolean(id),
    };
  });
}

export function useGitLog(projectId: () => string | undefined, limit = 30) {
  return createQuery(() => {
    const id = projectId();
    return {
      queryKey: ["git-log", id, limit],
      queryFn: () => gitLog(id as string, limit),
      enabled: Boolean(id),
    };
  });
}

export function useSessions(projectId: () => string | undefined) {
  return createQuery(() => {
    const id = projectId();
    return {
      queryKey: ["sessions", id],
      queryFn: () => listSessions(id as string),
      enabled: Boolean(id),
    };
  });
}

export function useSession(sessionId: () => string | undefined) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session", id],
      queryFn: () => getSession(id as string),
      enabled: Boolean(id),
      refetchInterval: (query) => {
        const state = query.state.data?.state;
        return state === "active" ? 1500 : false;
      },
    };
  });
}

export function useSessionContext(sessionId: () => string | undefined) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-context", id],
      queryFn: () => getSessionContext(id as string),
      enabled: Boolean(id),
    };
  });
}

export function useSessionTimeline(sessionId: () => string | undefined) {
  const queryClient = useQueryClient();
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-timeline", id],
      queryFn: () => getSessionTimeline(id as string, { limit: 100 }),
      enabled: Boolean(id),
      // Poll only while a turn is running. Constant 3s polling on idle sessions
      // burns requests and re-suspending the main surface for no benefit.
      refetchInterval: (query) => {
        const items = query.state.data?.items ?? [];
        const session = queryClient.getQueryData<SessionSummary>(["session", id]);
        return sessionTimelineRefetchInterval(items.length, session);
      },
    };
  });
}

export function useSessionDiff(
  sessionId: () => string | undefined,
  /** Diff is expensive (full Merkle walk). Only enable when the Diff pane is open. */
  enabled: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-diff", id],
      queryFn: () => getSessionDiff(id as string),
      enabled: Boolean(id) && enabled(),
    };
  });
}

export function useTurn(sessionId: () => string | undefined, turnId: () => string | undefined) {
  return createQuery(() => {
    const sid = sessionId();
    const tid = turnId();
    return {
      queryKey: ["turn", sid, tid],
      queryFn: () => getTurn(sid as string, tid as string),
      enabled: Boolean(sid) && Boolean(tid),
      refetchInterval: (query) => {
        const status = query.state.data?.status;
        if (
          status === "running" ||
          status === "waiting_for_job" ||
          status === "waiting_for_ask" ||
          status === "waiting_for_model" ||
          status === "canceling"
        ) {
          return 1500;
        }
        return false;
      },
    };
  });
}

export function useQueuedTurns(
  sessionId: () => string | undefined,
  activeTurnId: () => string | null | undefined,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["queued-turns", id],
      queryFn: () => getQueuedTurns(id as string),
      enabled: Boolean(id),
      // The queue only changes on send/cancel/promote, all of which emit SSE
      // events that invalidate this query. Poll only while a turn is active
      // as a fallback so the bar updates when promotion fires.
      refetchInterval: activeTurnId() ? 3000 : false,
    };
  });
}
