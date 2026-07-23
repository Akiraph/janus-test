import { type HealthResponse, healthResponseSchema } from "@janus/shared";
import { buildApiUrl } from "./api-client-core";

export async function getHealth(): Promise<HealthResponse> {
  const response = await fetch(buildApiUrl("/health/live"));
  const payload: unknown = await response.json();
  const version =
    typeof payload === "object" &&
    payload !== null &&
    "version" in payload &&
    typeof payload.version === "string"
      ? payload.version
      : "unknown";
  return healthResponseSchema.parse({
    service: "janus-server",
    status: "ok",
    version,
    time: new Date().toISOString(),
  });
}
