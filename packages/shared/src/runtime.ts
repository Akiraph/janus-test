import { z } from "zod";
import { isoDateTimeSchema } from "./common";
import { sandboxHardeningPolicySchema } from "./sandbox";
import { sessionStatusSchema } from "./sessions";

export const runtimeKindSchema = z.enum(["docker", "local_dev"]);
export type RuntimeKind = z.infer<typeof runtimeKindSchema>;

export const runtimeIsolationKindSchema = z.enum([
  "hardened_docker",
  "gvisor",
  "firecracker",
]);
export type RuntimeIsolationKind = z.infer<typeof runtimeIsolationKindSchema>;

export const runtimeCapabilityStatusSchema = z.enum([
  "active",
  "not_configured",
  "unavailable",
]);
export type RuntimeCapabilityStatus = z.infer<
  typeof runtimeCapabilityStatusSchema
>;

export const runtimeIsolationCapabilitySchema = z.object({
  kind: runtimeIsolationKindSchema,
  label: z.string().min(1),
  status: runtimeCapabilityStatusSchema,
  detail: z.string().min(1),
});
export type RuntimeIsolationCapability = z.infer<
  typeof runtimeIsolationCapabilitySchema
>;

export const runtimePolicyWarningCodeSchema = z.enum([
  "egress_allowlist_label_only",
  "egress_enforcement_dev_noop",
  "stronger_isolation_not_configured",
]);
export type RuntimePolicyWarningCode = z.infer<
  typeof runtimePolicyWarningCodeSchema
>;

export const runtimePolicyWarningSchema = z.object({
  code: runtimePolicyWarningCodeSchema,
  level: z.enum(["info", "warn", "error"]),
  message: z.string().min(1),
});
export type RuntimePolicyWarning = z.infer<typeof runtimePolicyWarningSchema>;

export const runtimeEgressModeSchema = z.enum(["enforced", "dev_noop"]);
export type RuntimeEgressMode = z.infer<typeof runtimeEgressModeSchema>;

export const runtimeEgressStatusSchema = z.object({
  mode: runtimeEgressModeSchema,
  allowedDestinations: z.array(z.string().min(1)),
  detail: z.string().min(1),
  warning: z.string().min(1).optional(),
});
export type RuntimeEgressStatus = z.infer<typeof runtimeEgressStatusSchema>;

export const sessionRuntimeSnapshotSchema = z.object({
  sessionId: z.string().min(1),
  sandboxId: z.string().min(1),
  runtime: runtimeKindSchema,
  hardening: sandboxHardeningPolicySchema,
  egress: runtimeEgressStatusSchema.optional(),
  isolation: z.array(runtimeIsolationCapabilitySchema),
  warnings: z.array(runtimePolicyWarningSchema),
  recordedAt: isoDateTimeSchema,
});
export type SessionRuntimeSnapshot = z.infer<
  typeof sessionRuntimeSnapshotSchema
>;

export const approvalRequestStatusSchema = z.enum([
  "pending",
  "approved",
  "denied",
]);
export type ApprovalRequestStatus = z.infer<typeof approvalRequestStatusSchema>;

export const approvalRequestSourceSchema = z.enum([
  "runtime_policy",
  "cli_adapter",
  "operator",
]);
export type ApprovalRequestSource = z.infer<typeof approvalRequestSourceSchema>;

export const approvalActionKindSchema = z.enum([
  "network_egress",
  "sandbox_capability",
  "credential_capability",
  "cli_permission",
  "runtime_policy",
]);
export type ApprovalActionKind = z.infer<typeof approvalActionKindSchema>;

export const approvalRiskLevelSchema = z.enum(["low", "medium", "high"]);
export type ApprovalRiskLevel = z.infer<typeof approvalRiskLevelSchema>;

export const runtimeApprovalRequestSchema = z.object({
  id: z.string().min(1),
  sessionId: z.string().min(1),
  source: approvalRequestSourceSchema,
  actionKind: approvalActionKindSchema,
  riskLevel: approvalRiskLevelSchema,
  title: z.string().min(1),
  description: z.string().min(1),
  status: approvalRequestStatusSchema,
  requestedAt: isoDateTimeSchema,
  decidedAt: isoDateTimeSchema.optional(),
  decisionNote: z.string().min(1).optional(),
});
export type RuntimeApprovalRequest = z.infer<
  typeof runtimeApprovalRequestSchema
>;

export const createRuntimeApprovalRequestSchema = z.object({
  source: approvalRequestSourceSchema.default("operator"),
  actionKind: approvalActionKindSchema,
  riskLevel: approvalRiskLevelSchema,
  title: z.string().trim().min(1).max(120),
  description: z.string().trim().min(1).max(1000),
});
export type CreateRuntimeApprovalRequestInput = z.input<
  typeof createRuntimeApprovalRequestSchema
>;
export type CreateRuntimeApprovalRequest = z.output<
  typeof createRuntimeApprovalRequestSchema
>;

export const resolveRuntimeApprovalRequestSchema = z.object({
  note: z.string().trim().min(1).max(500).optional(),
});
export type ResolveRuntimeApprovalRequest = z.infer<
  typeof resolveRuntimeApprovalRequestSchema
>;

export const sessionRuntimeHealthStatusSchema = z.enum([
  "not_started",
  "healthy",
  "warning",
  "failed",
  "completed",
]);
export type SessionRuntimeHealthStatus = z.infer<
  typeof sessionRuntimeHealthStatusSchema
>;

export const sessionRuntimeHealthSchema = z.object({
  sessionId: z.string().min(1),
  sessionStatus: sessionStatusSchema,
  status: sessionRuntimeHealthStatusSchema,
  failureCount: z.number().int().nonnegative(),
  latestFailure: z
    .object({
      message: z.string().min(1),
      timestamp: isoDateTimeSchema,
    })
    .optional(),
  policyWarningCount: z.number().int().nonnegative(),
  pendingApprovalCount: z.number().int().nonnegative(),
  updatedAt: isoDateTimeSchema,
});
export type SessionRuntimeHealth = z.infer<typeof sessionRuntimeHealthSchema>;

export const sessionRuntimeResponseSchema = z.object({
  health: sessionRuntimeHealthSchema,
  snapshot: sessionRuntimeSnapshotSchema.optional(),
  approvalRequests: z.array(runtimeApprovalRequestSchema),
});
export type SessionRuntimeResponse = z.infer<
  typeof sessionRuntimeResponseSchema
>;

export const listRuntimeApprovalRequestsResponseSchema = z.object({
  approvalRequests: z.array(runtimeApprovalRequestSchema),
});
export type ListRuntimeApprovalRequestsResponse = z.infer<
  typeof listRuntimeApprovalRequestsResponseSchema
>;

export const runtimeApprovalRequestResponseSchema = z.object({
  approvalRequest: runtimeApprovalRequestSchema,
});
export type RuntimeApprovalRequestResponse = z.infer<
  typeof runtimeApprovalRequestResponseSchema
>;
