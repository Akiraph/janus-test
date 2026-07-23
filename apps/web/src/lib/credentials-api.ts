import {
  type ConfigureCredentialRequest,
  type ConfigureCredentialResponse,
  configureCredentialResponseSchema,
  type ListCredentialsResponse,
  listCredentialsResponseSchema,
} from "@janus/shared";
import { requestJson } from "./api-client-core";

export function listCredentials(): Promise<ListCredentialsResponse> {
  return requestJson("/api/credentials", listCredentialsResponseSchema);
}

export function configureCredential(
  request: ConfigureCredentialRequest,
): Promise<ConfigureCredentialResponse> {
  return requestJson("/api/credentials", configureCredentialResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}
