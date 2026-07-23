import {
  type ActivityEvent,
  activityEventSchema,
  type BranchSessionRequest,
  type BranchSessionResponse,
  branchSessionResponseSchema,
  type CreateRuntimeApprovalRequestInput,
  type CreateSessionInput,
  type CreateSessionResponse,
  createRuntimeApprovalRequestSchema,
  createSessionResponseSchema,
  type DispatchInstructionRequest,
  type DispatchInstructionResponse,
  dispatchInstructionResponseSchema,
  type ListSessionApplyRecordsResponse,
  type ListSessionCheckpointsResponse,
  listSessionApplyRecordsResponseSchema,
  listSessionCheckpointsResponseSchema,
  type RenameSessionRequest,
  type RenameSessionResponse,
  type ResolveRuntimeApprovalRequest,
  renameSessionResponseSchema,
  resolveRuntimeApprovalRequestSchema,
  runtimeApprovalRequestResponseSchema,
  type SessionActivityResponse,
  type SessionApplyRecordResponse,
  type SessionDiff,
  type SessionRuntimeResponse,
  type StartSessionApplyRequest,
  sessionActivityResponseSchema,
  sessionApplyRecordResponseSchema,
  sessionDiffSchema,
  sessionRuntimeResponseSchema,
} from "@janus/shared";
import {
  requestJson,
  requestVoid,
  subscribeEventStream,
} from "./api-client-core";

export function createSession(
  request: CreateSessionInput,
): Promise<CreateSessionResponse> {
  return requestJson("/api/sessions", createSessionResponseSchema, {
    body: JSON.stringify(request),
    method: "POST",
  });
}

export function renameSession(
  sessionId: string,
  request: RenameSessionRequest,
): Promise<RenameSessionResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}`,
    renameSessionResponseSchema,
    {
      body: JSON.stringify(request),
      method: "PATCH",
    },
  );
}

export function deleteSession(sessionId: string): Promise<void> {
  return requestVoid(
    `/api/sessions/${encodeURIComponent(sessionId)}`,
    { method: "DELETE" },
    { fallbackMessage: "Failed to delete session." },
  );
}

export function dispatchInstruction(
  sessionId: string,
  request: DispatchInstructionRequest,
): Promise<DispatchInstructionResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/instructions`,
    dispatchInstructionResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function getSessionActivity(
  sessionId: string,
): Promise<SessionActivityResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/activity`,
    sessionActivityResponseSchema,
  );
}

export function subscribeSessionActivityStream(
  sessionId: string,
  onEvent: (event: ActivityEvent) => void,
  onError?: (error: unknown) => void,
): () => void {
  return subscribeEventStream(
    {
      eventName: "activity",
      path: `/api/sessions/${encodeURIComponent(sessionId)}/activity-stream`,
      schema: activityEventSchema,
    },
    onEvent,
    onError,
  );
}

export function getSessionRuntime(
  sessionId: string,
): Promise<SessionRuntimeResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/runtime`,
    sessionRuntimeResponseSchema,
  );
}

export function listSessionCheckpoints(
  sessionId: string,
): Promise<ListSessionCheckpointsResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/checkpoints`,
    listSessionCheckpointsResponseSchema,
  );
}

export function branchSession(
  sessionId: string,
  request: BranchSessionRequest,
): Promise<BranchSessionResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/branches`,
    branchSessionResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function listSessionApplyRecords(
  sessionId: string,
): Promise<ListSessionApplyRecordsResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/apply-records`,
    listSessionApplyRecordsResponseSchema,
  );
}

export function startSessionApply(
  sessionId: string,
  request: StartSessionApplyRequest = {},
): Promise<SessionApplyRecordResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/apply-records`,
    sessionApplyRecordResponseSchema,
    {
      body: JSON.stringify(request),
      method: "POST",
    },
  );
}

export function refreshSessionApplyReview(
  sessionId: string,
  applyId: string,
): Promise<SessionApplyRecordResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/apply-records/${encodeURIComponent(applyId)}/refresh`,
    sessionApplyRecordResponseSchema,
    { method: "POST" },
  );
}

export function applySessionApplyReview(
  sessionId: string,
  applyId: string,
): Promise<SessionApplyRecordResponse> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/apply-records/${encodeURIComponent(applyId)}/apply`,
    sessionApplyRecordResponseSchema,
    { method: "POST" },
  );
}

export function createRuntimeApprovalRequest(
  sessionId: string,
  request: CreateRuntimeApprovalRequestInput,
) {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/approval-requests`,
    runtimeApprovalRequestResponseSchema,
    {
      body: JSON.stringify(createRuntimeApprovalRequestSchema.parse(request)),
      method: "POST",
    },
  );
}

export function approveRuntimeApprovalRequest(
  sessionId: string,
  approvalRequestId: string,
  request: ResolveRuntimeApprovalRequest = {},
) {
  return resolveRuntimeApprovalRequest(
    sessionId,
    approvalRequestId,
    "approve",
    request,
  );
}

export function denyRuntimeApprovalRequest(
  sessionId: string,
  approvalRequestId: string,
  request: ResolveRuntimeApprovalRequest = {},
) {
  return resolveRuntimeApprovalRequest(
    sessionId,
    approvalRequestId,
    "deny",
    request,
  );
}

export function getSessionDiff(sessionId: string): Promise<SessionDiff> {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/diff`,
    sessionDiffSchema,
  );
}

function resolveRuntimeApprovalRequest(
  sessionId: string,
  approvalRequestId: string,
  decision: "approve" | "deny",
  request: ResolveRuntimeApprovalRequest,
) {
  return requestJson(
    `/api/sessions/${encodeURIComponent(sessionId)}/approval-requests/${encodeURIComponent(approvalRequestId)}/${decision}`,
    runtimeApprovalRequestResponseSchema,
    {
      body: JSON.stringify(resolveRuntimeApprovalRequestSchema.parse(request)),
      method: "POST",
    },
  );
}
