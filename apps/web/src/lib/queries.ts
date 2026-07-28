import { createQuery } from "@tanstack/solid-query";
import {
  getBootstrap,
  getMe,
  getProject,
  getProviders,
  getSession,
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
  listTerminals,
  type TerminalOwnerFilter,
} from "./api";

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

export function useSessionTimeline(sessionId: () => string | undefined) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-timeline", id],
      queryFn: () => getSessionTimeline(id as string, { limit: 100 }),
      enabled: Boolean(id),
      // Poll only while a turn is running. Constant 1.5s polling on idle sessions
      // was burning requests and re-suspending the main surface for no benefit.
      refetchInterval: (query) => {
        // Parent session query lives under ["session", id]; if we cannot see
        // activity from the timeline alone, fall back to a slow idle poll only
        // when the last page already has items (active conversation). Empty
        // timelines stay quiet until the user sends a message.
        const items = query.state.data?.items ?? [];
        if (items.length === 0) return false;
        return 3000;
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

export function useTerminals(
  owner: () => TerminalOwnerFilter | undefined,
  enabled: () => boolean = () => true,
) {
  return createQuery(() => {
    const filter = owner();
    return {
      queryKey: ["terminals", filter?.kind, filter?.id],
      queryFn: () => listTerminals(filter as TerminalOwnerFilter),
      enabled: Boolean(filter?.id) && enabled(),
    };
  });
}
