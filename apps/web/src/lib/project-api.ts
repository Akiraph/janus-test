import {
  type CommitGitChangesResponse,
  type ConnectProjectInput,
  type ConnectProjectResponse,
  commitGitChangesResponseSchema,
  connectProjectResponseSchema,
  type DeleteWorkspacePathRequest,
  type DeleteWorkspacePathResponse,
  deleteWorkspacePathResponseSchema,
  type GitHistoryResponse,
  type GitStatusResponse,
  gitHistoryResponseSchema,
  gitPathRequestSchema,
  gitStatusResponseSchema,
  type ListProjectsResponse,
  type ListProjectThreadsResponse,
  listProjectsResponseSchema,
  listProjectThreadsResponseSchema,
  type ProjectThreadsLiveEvent,
  projectThreadsLiveEventSchema,
  type RenameWorkspacePathRequest,
  type RenameWorkspacePathResponse,
  type RepoAuthorizationInput,
  type RepoAuthorizationResponse,
  renameWorkspacePathResponseSchema,
  repoAuthorizationResponseSchema,
  type WorkspaceFileResponse,
  type WorkspaceTreeResponse,
  type WriteWorkspaceFileResponse,
  workspaceFileResponseSchema,
  workspaceTreeResponseSchema,
  writeWorkspaceFileResponseSchema,
} from "@janus/shared";
import {
  buildApiUrl,
  requestJson,
  requestVoid,
  subscribeEventStream,
} from "./api-client-core";

export async function listProjects(): Promise<ListProjectsResponse> {
  const response = await fetch(buildApiUrl("/api/projects"), {
    credentials: "include",
  });
  if (response.status === 404) {
    return listProjectsResponseSchema.parse({ projects: [] });
  }
  const payload: unknown = await response.json();
  if (!response.ok) {
    throw new Error("Failed to load projects.");
  }
  return listProjectsResponseSchema.parse(payload);
}

export function connectProject(
  request: ConnectProjectInput,
): Promise<ConnectProjectResponse> {
  return requestJson("/api/projects", connectProjectResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}

export function deleteProject(projectId: string): Promise<void> {
  return requestVoid(
    `/api/projects/${encodeURIComponent(projectId)}`,
    { method: "DELETE" },
    {
      fallbackMessage: "Failed to delete project.",
      ignoreNotFound: true,
    },
  );
}

export function listProjectThreads(
  projectId: string,
): Promise<ListProjectThreadsResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/threads`,
    listProjectThreadsResponseSchema,
  );
}

export function subscribeProjectThreadsStream(
  projectId: string,
  onEvent: (event: ProjectThreadsLiveEvent) => void,
  onError?: (error: unknown) => void,
): () => void {
  return subscribeEventStream(
    {
      eventName: "threads",
      path: `/api/projects/${encodeURIComponent(projectId)}/threads-stream`,
      schema: projectThreadsLiveEventSchema,
    },
    onEvent,
    onError,
  );
}

export function getWorkspaceTree(
  projectId: string,
  path = "",
): Promise<WorkspaceTreeResponse> {
  const query = path.length > 0 ? `?path=${encodeURIComponent(path)}` : "";
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/tree${query}`,
    workspaceTreeResponseSchema,
  );
}

export function getWorkspaceFile(
  projectId: string,
  path: string,
): Promise<WorkspaceFileResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/file?path=${encodeURIComponent(path)}`,
    workspaceFileResponseSchema,
  );
}

export function writeWorkspaceFile(
  projectId: string,
  path: string,
  content: string,
): Promise<WriteWorkspaceFileResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/file`,
    writeWorkspaceFileResponseSchema,
    {
      body: JSON.stringify({ path, content }),
      method: "PUT",
    },
  );
}

export function renameWorkspacePath(
  projectId: string,
  request: RenameWorkspacePathRequest,
): Promise<RenameWorkspacePathResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/path`,
    renameWorkspacePathResponseSchema,
    {
      body: JSON.stringify(request),
      method: "PATCH",
    },
  );
}

export function deleteWorkspacePath(
  projectId: string,
  request: DeleteWorkspacePathRequest,
): Promise<DeleteWorkspacePathResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/path`,
    deleteWorkspacePathResponseSchema,
    {
      body: JSON.stringify(request),
      method: "DELETE",
    },
  );
}

export function listGitHistory(
  projectId: string,
  limit?: number,
): Promise<GitHistoryResponse> {
  const query = limit === undefined ? "" : `?limit=${limit}`;
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/commits${query}`,
    gitHistoryResponseSchema,
  );
}

export function getGitStatus(projectId: string): Promise<GitStatusResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/git/status`,
    gitStatusResponseSchema,
  );
}

export function stageGitFile(
  projectId: string,
  path: string,
): Promise<GitStatusResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/git/stage`,
    gitStatusResponseSchema,
    {
      body: JSON.stringify(gitPathRequestSchema.parse({ path })),
      method: "POST",
    },
  );
}

export function unstageGitFile(
  projectId: string,
  path: string,
): Promise<GitStatusResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/git/unstage`,
    gitStatusResponseSchema,
    {
      body: JSON.stringify(gitPathRequestSchema.parse({ path })),
      method: "POST",
    },
  );
}

export function stageAllGitFiles(
  projectId: string,
): Promise<GitStatusResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/git/stage-all`,
    gitStatusResponseSchema,
    { method: "POST" },
  );
}

export function commitGitChanges(
  projectId: string,
  message: string,
): Promise<CommitGitChangesResponse> {
  return requestJson(
    `/api/projects/${encodeURIComponent(projectId)}/git/commit`,
    commitGitChangesResponseSchema,
    {
      body: JSON.stringify({ message }),
      method: "POST",
    },
  );
}

export function authorizeRepository(
  request: RepoAuthorizationInput,
): Promise<RepoAuthorizationResponse> {
  return requestJson(
    "/api/repo-authorizations",
    repoAuthorizationResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}
