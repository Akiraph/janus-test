import type { components } from "../generated/api";

export type BootstrapResponse = components["schemas"]["BootstrapResponse"];
export type SystemInfoResponse = components["schemas"]["SystemInfoResponse"];

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

async function getJson<T>(path: string, decode: (value: unknown) => value is T): Promise<T> {
  const response = await fetch(path, {
    headers: { "X-Request-Id": crypto.randomUUID() },
  });
  if (!response.ok) {
    throw new ApiError(response.status, `Janus returned ${response.status}`);
  }
  const value: unknown = await response.json();
  if (!decode(value)) {
    throw new ApiError(502, "Janus returned an incompatible response");
  }
  return value;
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
