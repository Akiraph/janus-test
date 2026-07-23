/** Centralized query keys so mutations can target precise invalidations. */
export const workspaceKeys = {
  projectThreads: (projectId: string) =>
    ["project", projectId, "threads"] as const,
  workspaceTree: (projectId: string, path: string) =>
    ["project", projectId, "tree", path] as const,
  workspaceTreeScope: (projectId: string) =>
    ["project", projectId, "tree"] as const,
  workspaceFile: (projectId: string, path: string) =>
    ["project", projectId, "file", path] as const,
  workspaceFileScope: (projectId: string) =>
    ["project", projectId, "file"] as const,
  gitHistory: (projectId: string) => ["project", projectId, "commits"] as const,
  gitStatus: (projectId: string) =>
    ["project", projectId, "git-status"] as const,
  sessionRuns: (sessionId: string) => ["session", sessionId, "runs"] as const,
  sessionCheckpoints: (sessionId: string) =>
    ["session", sessionId, "checkpoints"] as const,
  sessionApplyRecords: (sessionId: string) =>
    ["session", sessionId, "apply-records"] as const,
  sessionActivity: (sessionId: string) =>
    ["session", sessionId, "activity"] as const,
  sessionRuntime: (sessionId: string) =>
    ["session", sessionId, "runtime"] as const,
  sessionDiff: (sessionId: string) => ["session", sessionId, "diff"] as const,
};
