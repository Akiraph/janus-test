import type { components } from "../generated/api";

export type BootstrapResponse = components["schemas"]["BootstrapResponse"];
export type SystemInfoResponse = components["schemas"]["SystemInfoResponse"];
export type CeremonyOptions = components["schemas"]["CeremonyOptions"];
export type OwnerView = components["schemas"]["OwnerView"];
export type ProviderView = components["schemas"]["ProviderView"];
export type ProviderInput = components["schemas"]["ProviderInput"];
export type ModelView = components["schemas"]["ModelView"];
export type ModelInput = components["schemas"]["ModelInput"];
export type PasskeyView = components["schemas"]["PasskeyView"];
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
export async function getModels(): Promise<ModelView[]> {
  return (await requestJson<{ data: ModelView[] }>("/api/v1/models", {}, isDataResponse)).data;
}
export async function createModel(input: ModelInput): Promise<ModelView> {
  return (
    await requestJson<{ data: ModelView }>(
      "/api/v1/models",
      { method: "POST", body: JSON.stringify(input) },
      isDataResponse,
    )
  ).data;
}
export async function deleteModel(id: string): Promise<void> {
  await requestJson(`/api/v1/models/${id}`, { method: "DELETE" }, () => true);
}

async function getJson<T>(path: string, decode: (value: unknown) => value is T): Promise<T> {
  return requestJson(path, { method: "GET" }, decode);
}

function requestInit(method = "GET", body?: string | object): RequestInit {
  const headers = new Headers({ "X-Request-Id": crypto.randomUUID(), Accept: "application/json" });
  if (csrfToken) headers.set("X-CSRF-Token", csrfToken);
  if (body !== undefined) {
    headers.set("Content-Type", "application/json");
    body = typeof body === "string" ? body : JSON.stringify(body);
  }
  const result: RequestInit = { method, headers, credentials: "include" };
  if (body !== undefined) result.body = body as BodyInit;
  return result;
}

async function requestJson<T>(
  path: string,
  init: { method?: string; body?: string },
  decode: (value: unknown) => boolean,
): Promise<T> {
  const response = await fetch(path, requestInit(init.method ?? "GET", init.body));
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

function hasData(value: unknown): boolean {
  return isRecord(value) && "data" in value;
}
const isDataResponse = hasData;
function rememberCsrf(value: string): void {
  csrfToken = value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
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
