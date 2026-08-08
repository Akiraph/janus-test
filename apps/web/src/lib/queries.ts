import { createQuery } from "@tanstack/solid-query";
import {
  getBootstrap,
  getMe,
  getProject,
  getProviders,
  getNotificationChannels,
  listSessionJobs,
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
} from "./api";

// Real-time state converges exclusively over the SSE stream: every resource the
// UI renders is projected to a complete `state` frame when it changes, and a
// reconnect replays missed projections (Last-Event-ID) then a full snapshot.
// No polling safety nets — they only masked a lossy push path that no longer
// exists, and they caused the stale-until-refetch flicker users had to refresh
// past. Terminal and operation state are still read on demand (they are not
// projected on the SSE channel).

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

export function useNotificationChannels() {
  return createQuery(() => ({
    queryKey: ["notification-channels"],
    queryFn: getNotificationChannels,
  }));
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

export function useSession(
  sessionId: () => string | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session", id],
      queryFn: () => getSession(id as string),
      enabled: Boolean(id) && shouldLoad(),
    };
  });
}

export function useSessionContext(
  sessionId: () => string | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-context", id],
      queryFn: () => getSessionContext(id as string),
      enabled: Boolean(id) && shouldLoad(),
    };
  });
}

export function useSessionTimeline(
  sessionId: () => string | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-timeline", id],
      queryFn: async () => {
        return getSessionTimeline(id as string, { limit: 100 });
      },
      placeholderData: (previous) => previous,
      enabled: Boolean(id) && shouldLoad(),
    };
  });
}

export function useSessionDiff(
  sessionId: () => string | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["session-diff", id],
      queryFn: () => getSessionDiff(id as string),
      enabled: Boolean(id) && shouldLoad(),
      placeholderData: (previous) => previous,
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
      placeholderData: (previous) => previous,
      enabled: Boolean(sid) && Boolean(tid),
    };
  });
}

export function useQueuedTurns(
  sessionId: () => string | undefined,
  _activeTurnId: () => string | null | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["queued-turns", id],
      queryFn: () => getQueuedTurns(id as string),
      enabled: Boolean(id) && shouldLoad(),
    };
  });
}

export function useSessionJobs(
  sessionId: () => string | undefined,
  shouldLoad: () => boolean = () => true,
) {
  return createQuery(() => {
    const id = sessionId();
    return {
      queryKey: ["jobs", id],
      queryFn: () => listSessionJobs(id as string),
      enabled: Boolean(id) && shouldLoad(),
      placeholderData: (previous) => previous,
    };
  });
}
