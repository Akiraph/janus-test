import type { components } from "../generated/api";

export type BootstrapResponse = components["schemas"]["BootstrapResponse"];
export type SystemInfoResponse = components["schemas"]["SystemInfoResponse"];
export type CeremonyOptions = components["schemas"]["CeremonyOptions"];
export type OwnerView = components["schemas"]["OwnerView"];
export type ProviderView = components["schemas"]["ProviderView"];
export type ProviderInput = components["schemas"]["ProviderInput"];
export type EmbeddedModelInput = components["schemas"]["EmbeddedModelInput"];
export type EmbeddedModelView = components["schemas"]["EmbeddedModelView"];
export type PasskeyView = components["schemas"]["PasskeyView"];
export type ProjectView = components["schemas"]["ProjectView"];
export type CreateProjectInput = components["schemas"]["CreateProjectInput"];
export type RetryProjectInput = components["schemas"]["RetryProjectInput"];
export type OperationView = components["schemas"]["OperationView"];
export type GithubCredentialView = components["schemas"]["GithubCredentialView"];
export type CreateGithubCredentialInput = components["schemas"]["CreateGithubCredentialInput"];
export type UpdateGithubCredentialInput = components["schemas"]["UpdateGithubCredentialInput"];
export type FileMetaView = components["schemas"]["FileMetaView"];
export type FileTreeView = components["schemas"]["FileTreeView"];
export type GitStatusView = components["schemas"]["GitStatusView"];
export type GitLogEntryView = components["schemas"]["GitLogEntryView"];
export type GitLogResponse = components["schemas"]["GitLogResponse"];
export type GitUpdateConflictView = components["schemas"]["GitUpdateConflictView"];
export type GitUpdateConflictPathView = components["schemas"]["GitUpdateConflictPathView"];
export type ResolveGitUpdateConflictInput = components["schemas"]["ResolveGitUpdateConflictInput"];
export type SaveTextInput = components["schemas"]["SaveTextInput"];
export type RepositoryInput = components["schemas"]["RepositoryInput"];
export type RepoAccess = components["schemas"]["RepoAccess"];
export type SessionSummary = components["schemas"]["SessionSummary"];
export type SessionModelPreference = components["schemas"]["SessionModelPreference"];
export type ReasoningEffort = components["schemas"]["ReasoningEffort"];
export type ContextUsageView = components["schemas"]["ContextUsageView"];
export type AttachmentView = components["schemas"]["AttachmentView"];
export type PublicLimits = components["schemas"]["PublicLimits"];
export type MessageRouteResult = components["schemas"]["MessageRouteResult"];
export type TimelinePage = components["schemas"]["TimelinePage"];
export type TimelineItemView = components["schemas"]["TimelineItemView"];
export type TurnSummary = components["schemas"]["TurnSummary"];
export type CreateSessionRequest = components["schemas"]["CreateSessionRequest"];
export type PostMessageRequest = components["schemas"]["PostMessageRequest"];
export type TerminalProjection = components["schemas"]["TerminalProjection"];
export type TerminalTicket = components["schemas"]["TerminalTicket"];
export type TerminalSizeInput = components["schemas"]["TerminalSizeInput"];
export type TerminalSignal = components["schemas"]["TerminalSignal"];
export type CreateTerminalRequest = components["schemas"]["CreateTerminalRequest"];
export type LogRange = components["schemas"]["LogRange"];
export type RuntimeCapability = components["schemas"]["RuntimeCapability"];
let csrfToken: string | undefined;

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export async function getBootstrap(): Promise<BootstrapResponse> {
  return getJson("/api/v1/bootstrap", isBootstrapResponse);
}

export async function getSystemInfo(): Promise<SystemInfoResponse> {
  return getJson("/api/v1/system/info", isSystemInfoResponse);
}

export async function getMe(): Promise<{ data: OwnerView }> {
  const response = await requestJson<{ data: OwnerView }>("/api/v1/me", { method: "GET" }, hasData);
  rememberCsrf(response.data.csrf_token);
  return response;
}

export async function initializeOptions(
  initialization_token: string,
  display_name: string,
): Promise<CeremonyOptions> {
  const response = await requestJson<{ data: CeremonyOptions }>(
    "/api/v1/auth/initialize/options",
    { method: "POST", body: JSON.stringify({ initialization_token, display_name }) },
    isDataResponse,
  );
  return response.data;
}

export async function initializeComplete(
  ceremony_id: string,
  credential: unknown,
): Promise<{ data: OwnerView; recoveryCodes: string[] }> {
  const response = await fetch(
    "/api/v1/auth/initialize/complete",
    requestInit("POST", { ceremony_id, credential }),
  );
  if (!response.ok) throw await toApiError(response);
  const codesHeader = response.headers.get("x-janus-recovery-codes");
  const result = (await response.json()) as { data: OwnerView };
  rememberCsrf(result.data.csrf_token);
  return { ...result, recoveryCodes: codesHeader ? (JSON.parse(codesHeader) as string[]) : [] };
}

export async function loginOptions(): Promise<CeremonyOptions> {
  const response = await requestJson<{ data: CeremonyOptions }>(
    "/api/v1/auth/passkey/options",
    { method: "POST" },
    isDataResponse,
  );
  return response.data;
}

export async function loginComplete(ceremony_id: string, credential: unknown): Promise<OwnerView> {
  const response = await requestJson<{ data: OwnerView }>(
    "/api/v1/auth/passkey/complete",
    { method: "POST", body: JSON.stringify({ ceremony_id, credential }) },
    isDataResponse,
  );
  rememberCsrf(response.data.csrf_token);
  return response.data;
}

export async function recoveryExchange(code: string): Promise<void> {
  await requestJson(
    "/api/v1/auth/recovery/exchange",
    { method: "POST", body: JSON.stringify({ code }) },
    hasData,
  );
}
export async function recoveryOptions(name: string): Promise<CeremonyOptions> {
  return (
    await requestJson<{ data: CeremonyOptions }>(
      "/api/v1/auth/recovery/passkey/options",
      { method: "POST", body: JSON.stringify({ name }) },
      hasData,
    )
  ).data;
}
export async function recoveryComplete(
  ceremony_id: string,
  credential: unknown,
): Promise<OwnerView> {
  const response = await requestJson<{ data: OwnerView }>(
    "/api/v1/auth/recovery/passkey/complete",
    { method: "POST", body: JSON.stringify({ ceremony_id, credential }) },
    hasData,
  );
  rememberCsrf(response.data.csrf_token);
  return response.data;
}

export async function logout(): Promise<void> {
  await requestJson("/api/v1/auth/logout", { method: "POST" }, () => true);
}
export async function getProviders(): Promise<ProviderView[]> {
  return (
    await requestJson<{ data: ProviderView[] }>("/api/v1/model-providers", {}, isDataResponse)
  ).data;
}
export async function createProvider(input: ProviderInput): Promise<ProviderView> {
  return (
    await requestJson<{ data: ProviderView }>(
      "/api/v1/model-providers",
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}
export async function updateProvider(id: string, input: ProviderInput): Promise<ProviderView> {
  return (
    await requestJson<{ data: ProviderView }>(
      `/api/v1/model-providers/${id}`,
      { method: "PATCH", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}
export async function deleteProvider(id: string): Promise<void> {
  await requestJson(`/api/v1/model-providers/${id}`, { method: "DELETE" }, () => true);
}
export async function probeProvider(id: string): Promise<components["schemas"]["ProbeResult"]> {
  return (
    await requestJson<{ data: components["schemas"]["ProbeResult"] }>(
      `/api/v1/model-providers/${id}/probe`,
      { method: "POST" },
      isDataResponse,
    )
  ).data;
}

export async function listProjects(limit = 50): Promise<ProjectView[]> {
  const query = new URLSearchParams({ limit: String(limit) });
  return (
    await requestJson<{ data: ProjectView[] }>(
      `/api/v1/projects?${query}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function createProject(
  input: CreateProjectInput,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      "/api/v1/projects",
      {
        method: "POST",
        body: JSON.stringify(input),
        headers: { "Idempotency-Key": idempotencyKey },
      },
      isDataResponse,
    )
  ).data;
}

export async function getProject(id: string): Promise<ProjectView> {
  return (
    await requestJson<{ data: ProjectView }>(
      `/api/v1/projects/${id}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function listSessions(projectId: string, limit = 50): Promise<SessionSummary[]> {
  const query = new URLSearchParams({ limit: String(limit) });
  return (
    await requestJson<{ data: SessionSummary[] }>(
      `/api/v1/projects/${projectId}/sessions?${query}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function createSession(
  projectId: string,
  input: CreateSessionRequest = {},
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${projectId}/sessions`,
      {
        method: "POST",
        body: JSON.stringify(input),
        headers: { "Idempotency-Key": idempotencyKey },
      },
      isDataResponse,
    )
  ).data;
}

export async function getSession(id: string): Promise<SessionSummary> {
  return (
    await requestJson<{ data: SessionSummary }>(
      `/api/v1/sessions/${id}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function deleteSession(
  id: string,
  ifMatch: string,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/sessions/${id}`,
      {
        method: "DELETE",
        headers: {
          "If-Match": ifMatch,
          "Idempotency-Key": idempotencyKey,
        },
      },
      isDataResponse,
    )
  ).data;
}

export async function postSessionMessage(
  id: string,
  input: PostMessageRequest,
): Promise<MessageRouteResult> {
  return (
    await requestJson<{ data: MessageRouteResult }>(
      `/api/v1/sessions/${id}/messages`,
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}

export async function uploadSessionAttachment(id: string, file: File): Promise<AttachmentView> {
  const query = new URLSearchParams({ name: file.name });
  const response = await fetch(`/api/v1/sessions/${id}/attachments?${query}`, {
    method: "POST",
    headers: requestHeaders({ "Content-Type": file.type || "application/octet-stream" }),
    credentials: "include",
    body: file,
  });
  if (!response.ok) throw await toApiError(response);
  const value: unknown = await response.json();
  if (!hasData(value)) throw new ApiError(502, "Janus returned an incompatible response");
  return value.data as AttachmentView;
}

export async function deleteSessionAttachment(id: string, attachmentId: string): Promise<void> {
  await requestJson(
    `/api/v1/sessions/${id}/attachments/${attachmentId}`,
    { method: "DELETE" },
    () => true,
  );
}

export async function getSessionContext(id: string): Promise<ContextUsageView | null> {
  return (
    await requestJson<{ data: ContextUsageView | null }>(
      `/api/v1/sessions/${id}/context`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function getSessionTimeline(
  id: string,
  opts: { before?: string; after?: string; limit?: number } = {},
): Promise<TimelinePage> {
  const query = new URLSearchParams();
  if (opts.before) query.set("before", opts.before);
  if (opts.after) query.set("after", opts.after);
  if (opts.limit) query.set("limit", String(opts.limit));
  const suffix = query.toString() ? `?${query}` : "";
  return (
    await requestJson<{ data: TimelinePage }>(
      `/api/v1/sessions/${id}/timeline${suffix}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function getSessionDiff(id: string): Promise<Record<string, unknown>> {
  return (
    await requestJson<{ data: Record<string, unknown> }>(
      `/api/v1/sessions/${id}/diff`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function getTurn(sessionId: string, turnId: string): Promise<TurnSummary> {
  return (
    await requestJson<{ data: TurnSummary }>(
      `/api/v1/sessions/${sessionId}/turns/${turnId}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function listTerminals(projectId: string): Promise<TerminalProjection[]> {
  const query = new URLSearchParams({ project_id: projectId });
  return (
    await requestJson<{ data: TerminalProjection[] }>(
      `/api/v1/terminals?${query}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function createTerminal(input: CreateTerminalRequest): Promise<TerminalProjection> {
  return (
    await requestJson<{ data: TerminalProjection }>(
      "/api/v1/terminals",
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}

export async function issueTerminalTicket(id: string): Promise<TerminalTicket> {
  // Ticket issuance is Origin-bound. Browsers set Origin automatically for
  // same-origin fetch; keep credentials so the session cookie travels too.
  return (
    await requestJson<{ data: TerminalTicket }>(
      `/api/v1/terminals/${id}/tickets`,
      { method: "POST" },
      isDataResponse,
    )
  ).data;
}

export async function getTerminalScrollback(
  id: string,
  opts: { after?: string; limit?: number } = {},
): Promise<LogRange> {
  const query = new URLSearchParams();
  if (opts.after) query.set("after", opts.after);
  if (opts.limit) query.set("limit", String(opts.limit));
  const suffix = query.toString() ? `?${query}` : "";
  return (
    await requestJson<{ data: LogRange }>(
      `/api/v1/terminals/${id}/scrollback${suffix}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function resizeTerminal(
  id: string,
  size: TerminalSizeInput,
): Promise<TerminalProjection> {
  return (
    await requestJson<{ data: TerminalProjection }>(
      `/api/v1/terminals/${id}/resize`,
      { method: "POST", body: JSON.stringify(size) },
      isDataResponse,
    )
  ).data;
}

export async function signalTerminal(id: string, signal: TerminalSignal): Promise<void> {
  await requestJson(
    `/api/v1/terminals/${id}/signal`,
    { method: "POST", body: JSON.stringify({ signal }) },
    () => true,
  );
}

export async function closeTerminal(id: string): Promise<TerminalProjection> {
  return (
    await requestJson<{ data: TerminalProjection }>(
      `/api/v1/terminals/${id}/close`,
      { method: "POST" },
      isDataResponse,
    )
  ).data;
}

/** Build the WebSocket URL for a Terminal connect upgrade (ticket token in query). */
export function terminalConnectUrl(id: string, token: string, after?: string | null): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const query = new URLSearchParams({ token });
  if (after) query.set("after", after);
  return `${protocol}//${window.location.host}/api/v1/terminals/${id}/connect?${query}`;
}

export async function retryProject(
  id: string,
  input: RetryProjectInput = {},
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${id}/retry`,
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}

export async function deleteProject(
  id: string,
  ifMatch: string,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${id}`,
      {
        method: "DELETE",
        headers: {
          "If-Match": ifMatch,
          "Idempotency-Key": idempotencyKey,
        },
      },
      isDataResponse,
    )
  ).data;
}

export async function listGithubCredentials(): Promise<GithubCredentialView[]> {
  return (
    await requestJson<{ data: GithubCredentialView[] }>(
      "/api/v1/github-credentials",
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function createGithubCredential(
  input: CreateGithubCredentialInput,
): Promise<GithubCredentialView> {
  return (
    await requestJson<{ data: GithubCredentialView }>(
      "/api/v1/github-credentials",
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}

export async function updateGithubCredential(
  id: string,
  ifMatch: string,
  input: UpdateGithubCredentialInput,
): Promise<GithubCredentialView> {
  return (
    await requestJson<{ data: GithubCredentialView }>(
      `/api/v1/github-credentials/${id}`,
      {
        method: "PATCH",
        body: JSON.stringify(input),
        headers: { "If-Match": ifMatch },
      },
      isDataResponse,
    )
  ).data;
}

export async function deleteGithubCredential(id: string): Promise<void> {
  await requestJson(`/api/v1/github-credentials/${id}`, { method: "DELETE" }, () => true);
}

export async function listFileTree(projectId: string, path?: string): Promise<FileTreeView[]> {
  const query = new URLSearchParams();
  if (path) query.set("path", path);
  const suffix = query.size > 0 ? `?${query}` : "";
  return (
    await requestJson<{ data: FileTreeView[] }>(
      `/api/v1/projects/${projectId}/files/tree${suffix}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function getFileMeta(projectId: string, path: string): Promise<FileMetaView> {
  const query = new URLSearchParams({ path });
  return (
    await requestJson<{ data: FileMetaView }>(
      `/api/v1/projects/${projectId}/files/meta?${query}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

/** Raw file bytes decoded as text. CodeMirror is optional; M2 uses native controls. */
export async function getFileContentText(projectId: string, path: string): Promise<string> {
  const query = new URLSearchParams({ path });
  const response = await fetch(
    `/api/v1/projects/${projectId}/files/content?${query}`,
    requestInit("GET"),
  );
  if (!response.ok) throw await toApiError(response);
  return response.text();
}

export async function saveFileText(projectId: string, input: SaveTextInput): Promise<string> {
  return (
    await requestJson<{ data: string }>(
      `/api/v1/projects/${projectId}/files/text`,
      { method: "PUT", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}

export async function gitStatus(projectId: string): Promise<GitStatusView> {
  return (
    await requestJson<{ data: GitStatusView }>(
      `/api/v1/projects/${projectId}/git/status`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function gitLog(projectId: string, limit = 50): Promise<GitLogEntryView[]> {
  const query = new URLSearchParams({ limit: String(limit) });
  return (
    await requestJson<{ data: GitLogResponse }>(
      `/api/v1/projects/${projectId}/git/log?${query}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data.entries;
}

export async function gitBranches(projectId: string): Promise<string[]> {
  return (
    await requestJson<{ data: string[] }>(
      `/api/v1/projects/${projectId}/git/branches`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function gitRemotes(projectId: string): Promise<string[]> {
  return (
    await requestJson<{ data: string[] }>(
      `/api/v1/projects/${projectId}/git/remotes`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function gitStage(projectId: string, paths: string[]): Promise<void> {
  await requestJson(
    `/api/v1/projects/${projectId}/git/commands/stage`,
    { method: "POST", body: JSON.stringify({ paths }) },
    () => true,
  );
}

export async function gitUnstage(projectId: string, paths: string[]): Promise<void> {
  await requestJson(
    `/api/v1/projects/${projectId}/git/commands/unstage`,
    { method: "POST", body: JSON.stringify({ paths }) },
    () => true,
  );
}

export async function gitCommit(projectId: string, message: string): Promise<string> {
  return (
    await requestJson<{ data: string }>(
      `/api/v1/projects/${projectId}/git/commands/commit`,
      { method: "POST", body: JSON.stringify({ message }) },
      isDataResponse,
    )
  ).data;
}

export async function gitFetch(
  projectId: string,
  remote: string,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${projectId}/git/commands/fetch`,
      {
        method: "POST",
        body: JSON.stringify({ remote }),
        headers: { "Idempotency-Key": idempotencyKey },
      },
      isDataResponse,
    )
  ).data;
}

export async function gitPush(
  projectId: string,
  remote: string,
  branch: string,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${projectId}/git/commands/push`,
      {
        method: "POST",
        body: JSON.stringify({ remote, branch }),
        headers: { "Idempotency-Key": idempotencyKey },
      },
      isDataResponse,
    )
  ).data;
}

export async function gitUpdate(
  projectId: string,
  remote: string,
  branch: string,
  idempotencyKey: string,
): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/projects/${projectId}/git/commands/update`,
      {
        method: "POST",
        body: JSON.stringify({ remote, branch }),
        headers: { "Idempotency-Key": idempotencyKey },
      },
      isDataResponse,
    )
  ).data;
}

export async function listGitUpdateConflicts(projectId: string): Promise<GitUpdateConflictView[]> {
  return (
    await requestJson<{ data: GitUpdateConflictView[] }>(
      `/api/v1/projects/${projectId}/git/update-conflicts`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function resolveGitUpdateConflict(
  projectId: string,
  conflictId: string,
  ifMatch: string,
  input: ResolveGitUpdateConflictInput,
): Promise<GitUpdateConflictView> {
  return (
    await requestJson<{ data: GitUpdateConflictView }>(
      `/api/v1/projects/${projectId}/git/update-conflicts/${conflictId}/resolve`,
      {
        method: "POST",
        body: JSON.stringify(input),
        headers: { "If-Match": ifMatch },
      },
      isDataResponse,
    )
  ).data;
}

export async function getOperation(id: string): Promise<OperationView> {
  return (
    await requestJson<{ data: OperationView }>(
      `/api/v1/operations/${id}`,
      { method: "GET" },
      isDataResponse,
    )
  ).data;
}

export async function waitForOperation(
  id: string,
  timeoutMs = 120_000,
  pollIntervalMs = 250,
): Promise<OperationView> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    const operation = await getOperation(id);
    if (operation.status === "succeeded") return operation;
    if (
      operation.status === "failed" ||
      operation.status === "canceled" ||
      operation.status === "needs_attention"
    ) {
      throw new ApiError(409, operationFailureMessage(operation.problem));
    }
    if (Date.now() >= deadline) {
      throw new ApiError(504, `Operation ${id} did not finish in time`);
    }
    await new Promise<void>((resolve) => setTimeout(resolve, pollIntervalMs));
  }
}

async function getJson<T>(path: string, decode: (value: unknown) => value is T): Promise<T> {
  return requestJson(path, { method: "GET" }, decode);
}

function requestInit(
  method = "GET",
  body?: string | object,
  extraHeaders?: Record<string, string>,
): RequestInit {
  const headers = requestHeaders(extraHeaders);
  if (body !== undefined) {
    headers.set("Content-Type", "application/json");
    body = typeof body === "string" ? body : JSON.stringify(body);
  }
  const result: RequestInit = { method, headers, credentials: "include" };
  if (body !== undefined) result.body = body as BodyInit;
  return result;
}

function requestHeaders(extraHeaders?: Record<string, string>): Headers {
  const headers = new Headers({ "X-Request-Id": crypto.randomUUID(), Accept: "application/json" });
  if (csrfToken) headers.set("X-CSRF-Token", csrfToken);
  if (extraHeaders) {
    for (const [key, value] of Object.entries(extraHeaders)) headers.set(key, value);
  }
  return headers;
}

async function requestJson<T>(
  path: string,
  init: { method?: string; body?: string; headers?: Record<string, string> },
  decode: (value: unknown) => boolean,
): Promise<T> {
  const response = await fetch(path, requestInit(init.method ?? "GET", init.body, init.headers));
  if (!response.ok) throw await toApiError(response);
  if (response.status === 204) return undefined as T;
  const value: unknown = await response.json();
  if (!decode(value)) throw new ApiError(502, "Janus returned an incompatible response");
  return value as T;
}

async function toApiError(response: Response): Promise<ApiError> {
  const value = (await response.json().catch(() => undefined)) as
    | { detail?: string; code?: string }
    | undefined;
  return new ApiError(
    response.status,
    value?.detail ?? value?.code ?? `Janus returned ${response.status}`,
  );
}

function hasData(value: unknown): value is { data: unknown } {
  return isRecord(value) && "data" in value;
}
const isDataResponse = hasData;
function rememberCsrf(value: string): void {
  csrfToken = value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function operationFailureMessage(problem: unknown): string {
  if (!isRecord(problem)) return "Operation failed";
  for (const key of ["detail", "title", "code"] as const) {
    if (typeof problem[key] === "string") return problem[key];
  }
  return "Operation failed";
}

function isBootstrapResponse(value: unknown): value is BootstrapResponse {
  if (!isRecord(value) || !isRecord(value.data)) return false;
  const { data } = value;
  return (
    (data.state === "uninitialized" || data.state === "initialized") &&
    typeof data.development_auth === "boolean" &&
    typeof data.webauthn_rp_name === "string" &&
    typeof data.version === "string" &&
    isRecord(data.limits)
  );
}

function isSystemInfoResponse(value: unknown): value is SystemInfoResponse {
  if (!isRecord(value) || !isRecord(value.data)) return false;
  const { data } = value;
  return (
    typeof data.version === "string" &&
    typeof data.schema_version === "number" &&
    typeof data.mode === "string" &&
    isRecord(data.database) &&
    isRecord(data.events) &&
    Array.isArray(data.capabilities)
  );
}
