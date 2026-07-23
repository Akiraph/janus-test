import {
  type StartEmptySandboxInput,
  type StartEmptySandboxResponse,
  startEmptySandboxResponseSchema,
} from "@janus/shared";
import { requestJson } from "./api-client-core";

export function prepareEmptySandbox(
  request: StartEmptySandboxInput,
): Promise<StartEmptySandboxResponse> {
  return requestJson("/api/sandboxes/empty", startEmptySandboxResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}
