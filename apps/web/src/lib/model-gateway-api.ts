import {
  activeModelGatewayRouteResponseSchema,
  type ListModelProvidersResponse,
  listModelProvidersResponseSchema,
  type ModelGatewayStatusResponse,
  type ModelProviderResponse,
  modelGatewayStatusResponseSchema,
  modelProviderResponseSchema,
  type SetActiveModelGatewayRouteInput,
  type TestModelProviderInput,
  type TestModelProviderResponse,
  testModelProviderResponseSchema,
  type UpsertModelProviderInput,
} from "@janus/shared";
import { requestJson, requestVoid } from "./api-client-core";

export function listModelProviders(): Promise<ListModelProvidersResponse> {
  return requestJson(
    "/api/model-gateway/providers",
    listModelProvidersResponseSchema,
  );
}

export function upsertModelProvider(
  request: UpsertModelProviderInput,
): Promise<ModelProviderResponse> {
  return requestJson(
    "/api/model-gateway/providers",
    modelProviderResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function testModelProvider(
  request: TestModelProviderInput,
): Promise<TestModelProviderResponse> {
  return requestJson(
    "/api/model-gateway/providers/test",
    testModelProviderResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function deleteModelProvider(providerId: string): Promise<void> {
  return requestVoid(
    `/api/model-gateway/providers/${encodeURIComponent(providerId)}`,
    { method: "DELETE" },
    { fallbackMessage: "Failed to delete model provider." },
  );
}

export function setActiveModelGatewayRoute(
  request: SetActiveModelGatewayRouteInput,
) {
  return requestJson(
    "/api/model-gateway/active-route",
    activeModelGatewayRouteResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function getModelGatewayStatus(): Promise<ModelGatewayStatusResponse> {
  return requestJson(
    "/api/model-gateway/status",
    modelGatewayStatusResponseSchema,
  );
}
