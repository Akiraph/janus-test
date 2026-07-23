import {
  type DeliverQueuedSupervisorRunResponse,
  deliverQueuedSupervisorRunResponseSchema,
  type GetSupervisorRunResponse,
  getSupervisorRunResponseSchema,
  type ListSupervisorRunsResponse,
  listSupervisorRunsResponseSchema,
  type ResolveSupervisorAskRequest,
  type ResolveSupervisorAskResponse,
  resolveSupervisorAskRequestSchema,
  resolveSupervisorAskResponseSchema,
  type StartSupervisorRunInput,
  type StartSupervisorRunResponse,
  type SupervisorRunLiveEvent,
  startSupervisorRunResponseSchema,
  supervisorRunLiveEventSchema,
} from "@janus/shared";
import {
  requestJson,
  requestVoid,
  subscribeEventStream,
} from "./api-client-core";

export function listSupervisorRuns(
  sessionId: string,
): Promise<ListSupervisorRunsResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/runs`,
    listSupervisorRunsResponseSchema,
  );
}

export function subscribeSupervisorRunStream(
  sessionId: string,
  onEvent: (event: SupervisorRunLiveEvent) => void,
  onError?: (error: unknown) => void,
): () => void {
  return subscribeEventStream(
    {
      eventName: "run",
      path: `/api/sessions/${encodeURIComponent(sessionId)}/runs-stream`,
      schema: supervisorRunLiveEventSchema,
    },
    onEvent,
    onError,
  );
}

export function startSupervisorRun(
  request: StartSupervisorRunInput,
): Promise<StartSupervisorRunResponse> {
  return requestJson("/api/supervisor-runs", startSupervisorRunResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}

export function getSupervisorRun(
  runId: string,
): Promise<GetSupervisorRunResponse> {
  return requestJson(
    `/api/supervisor-runs/${encodeURIComponent(runId)}`,
    getSupervisorRunResponseSchema,
  );
}

export function cancelSupervisorRun(runId: string): Promise<void> {
  return requestVoid(
    `/api/supervisor-runs/${encodeURIComponent(runId)}/cancel`,
    { method: "POST" },
    { fallbackMessage: "Failed to cancel supervisor run." },
  );
}

export function deliverQueuedSupervisorRun(
  runId: string,
): Promise<DeliverQueuedSupervisorRunResponse> {
  return requestJson(
    `/api/supervisor-runs/${encodeURIComponent(runId)}/deliver`,
    deliverQueuedSupervisorRunResponseSchema,
    {
      method: "POST",
    },
  );
}

export function answerSupervisorAsk(
  runId: string,
  askId: string,
  request: ResolveSupervisorAskRequest,
): Promise<ResolveSupervisorAskResponse> {
  return requestJson(
    `/api/supervisor-runs/${encodeURIComponent(runId)}/asks/${encodeURIComponent(askId)}/answer`,
    resolveSupervisorAskResponseSchema,
    {
      body: JSON.stringify(resolveSupervisorAskRequestSchema.parse(request)),
      method: "POST",
    },
  );
}
