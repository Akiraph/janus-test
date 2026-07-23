import {
  type AuthStatusResponse,
  authStatusResponseSchema,
  type LoginRequest,
  type UpdateOwnerCredentialsRequest,
  type UpdateOwnerPasswordRequest,
  type UpdateOwnerUsernameRequest,
} from "@janus/shared";
import { buildApiUrl, requestJson } from "./api-client-core";

export async function getAuthStatus(): Promise<AuthStatusResponse> {
  const bootstrap = await fetch(buildApiUrl("/api/v1/bootstrap"), {
    credentials: "include",
  });
  const payload: unknown = await bootstrap.json();
  const developmentAuth =
    typeof payload === "object" &&
    payload !== null &&
    "data" in payload &&
    typeof payload.data === "object" &&
    payload.data !== null &&
    "development_auth" in payload.data &&
    payload.data.development_auth === true;

  if (developmentAuth) {
    return authStatusResponseSchema.parse({
      authenticated: true,
      user: { username: "owner", requiresCredentialSetup: false },
    });
  }

  const me = await fetch(buildApiUrl("/api/v1/me"), { credentials: "include" });
  return authStatusResponseSchema.parse({
    authenticated: me.ok,
    ...(me.ok
      ? { user: { username: "owner", requiresCredentialSetup: false } }
      : {}),
  });
}

export function login(request: LoginRequest): Promise<AuthStatusResponse> {
  return requestJson("/api/auth/login", authStatusResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}

export function logout(): Promise<AuthStatusResponse> {
  return requestJson("/api/auth/logout", authStatusResponseSchema, {
    method: "POST",
  });
}

export function updateOwnerCredentials(
  request: UpdateOwnerCredentialsRequest,
): Promise<AuthStatusResponse> {
  return requestJson("/api/auth/credentials", authStatusResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}

export function updateOwnerUsername(
  request: UpdateOwnerUsernameRequest,
): Promise<AuthStatusResponse> {
  return requestJson("/api/auth/username", authStatusResponseSchema, {
    body: JSON.stringify(request),
    method: "PATCH",
  });
}

export function updateOwnerPassword(
  request: UpdateOwnerPasswordRequest,
): Promise<AuthStatusResponse> {
  return requestJson("/api/auth/password", authStatusResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}
