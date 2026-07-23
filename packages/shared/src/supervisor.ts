import { z } from "zod";
import { cliKindSchema } from "./cli";
import { isoDateTimeSchema } from "./common";
import { routeModelAliasSchema } from "./model-gateway";
import { sessionDiffSchema } from "./sessions";

export const supervisorToolNames = [
  "todo",
  "bash",
  "read_file",
  "write_file",
  "edit_file",
  "dispatch_claude_code",
  "dispatch_codex",
  "read_cli_job",
  "stop_cli_job",
  "group_discussion",
  "read_group_discussion",
  "ask_user",
  "browser_action",
] as const;
export const supervisorToolNameSchema = z.enum(supervisorToolNames);
export type SupervisorToolName = z.infer<typeof supervisorToolNameSchema>;

export const supervisorRunAttachmentMaxBytes = 10 * 1024 * 1024;
export const supervisorRunAttachmentMaxCount = 10;
export const supervisorRunTaskMaxChars = 100_000;
const maxAttachmentBase64Chars =
  Math.ceil(supervisorRunAttachmentMaxBytes / 3) * 4;

const base64ContentSchema = z
  .string()
  .max(maxAttachmentBase64Chars)
  .regex(/^[A-Za-z0-9+/]*={0,2}$/);

export const supervisorRunAttachmentInputSchema = z
  .object({
    name: z.string().trim().min(1).max(240),
    mediaType: z.string().trim().min(1).max(160).optional(),
    sizeBytes: z
      .number()
      .int()
      .nonnegative()
      .max(supervisorRunAttachmentMaxBytes),
    contentBase64: base64ContentSchema,
  })
  .strict();
export type SupervisorRunAttachmentInput = z.infer<
  typeof supervisorRunAttachmentInputSchema
>;

export const supervisorRunAttachmentRecordSchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1).max(240),
  mediaType: z.string().min(1).max(160).optional(),
  sizeBytes: z
    .number()
    .int()
    .nonnegative()
    .max(supervisorRunAttachmentMaxBytes),
  path: z.string().min(1),
  uploadedAt: isoDateTimeSchema,
});
export type SupervisorRunAttachmentRecord = z.infer<
  typeof supervisorRunAttachmentRecordSchema
>;

export const supervisorBestOfNRequestSchema = z
  .object({
    candidateCount: z.number().int().min(1).max(5).default(3),
  })
  .strict();
export type SupervisorBestOfNRequest = z.infer<
  typeof supervisorBestOfNRequestSchema
>;

export const supervisorBestOfNCandidateStatusSchema = z.enum([
  "completed",
  "failed",
]);
export type SupervisorBestOfNCandidateStatus = z.infer<
  typeof supervisorBestOfNCandidateStatusSchema
>;

export const supervisorBestOfNCandidateRecordSchema = z.object({
  id: z.string().min(1),
  index: z.number().int().min(0).max(4),
  status: supervisorBestOfNCandidateStatusSchema,
  score: z.number(),
  summary: z.string().min(1).max(500).optional(),
  error: z.string().min(1).max(500).optional(),
});
export type SupervisorBestOfNCandidateRecord = z.infer<
  typeof supervisorBestOfNCandidateRecordSchema
>;

export const supervisorBestOfNRunRecordSchema = z.object({
  candidateCount: z.number().int().min(1).max(5),
  selectedCandidateId: z.string().min(1).optional(),
  candidates: z.array(supervisorBestOfNCandidateRecordSchema).default([]),
});
export type SupervisorBestOfNRunRecord = z.infer<
  typeof supervisorBestOfNRunRecordSchema
>;

export const supervisorAskRequestStatusSchema = z.enum([
  "pending",
  "answered",
  "cancelled",
]);
export type SupervisorAskRequestStatus = z.infer<
  typeof supervisorAskRequestStatusSchema
>;

export const supervisorAskRequestRecordSchema = z.object({
  id: z.string().min(1),
  toolUseId: z.string().min(1),
  question: z.string().min(1).max(2000),
  context: z.string().min(1).max(4000).optional(),
  options: z.array(z.string().min(1).max(240)).min(1).max(8).optional(),
  status: supervisorAskRequestStatusSchema,
  requestedAt: isoDateTimeSchema,
  answer: z.string().min(1).max(8000).optional(),
  answeredAt: isoDateTimeSchema.optional(),
});
export type SupervisorAskRequestRecord = z.infer<
  typeof supervisorAskRequestRecordSchema
>;

export const resolveSupervisorAskRequestSchema = z
  .object({
    answer: z.string().trim().min(1).max(8000),
  })
  .strict();
export type ResolveSupervisorAskRequest = z.infer<
  typeof resolveSupervisorAskRequestSchema
>;

export const supervisorCliAccessSchema = z.enum(["read-only", "full-access"]);
export type SupervisorCliAccess = z.infer<typeof supervisorCliAccessSchema>;

export const supervisorCliEffortSchema = z.enum([
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultracode",
]);
export type SupervisorCliEffort = z.infer<typeof supervisorCliEffortSchema>;

export const supervisorCliLaunchOptionsSchema = z
  .object({
    model: z
      .string()
      .trim()
      .min(1)
      .max(100)
      .regex(/^[A-Za-z0-9._:/+-]+$/)
      .optional(),
    effort: supervisorCliEffortSchema.optional(),
    access: supervisorCliAccessSchema.default("full-access"),
  })
  .strict();

export type SupervisorCliLaunchOptions = z.infer<
  typeof supervisorCliLaunchOptionsSchema
>;

export const supervisorCliJobStatusSchema = z.enum([
  "running",
  "completed",
  "failed",
  "canceled",
]);
export type SupervisorCliJobStatus = z.infer<
  typeof supervisorCliJobStatusSchema
>;

export const supervisorCliJobDetailSchema = z.enum([
  "summary",
  "standard",
  "verbose",
]);
export type SupervisorCliJobDetail = z.infer<
  typeof supervisorCliJobDetailSchema
>;

export const supervisorCliJobCanceledReasonSchema = z.enum([
  "run_canceled",
  "requirements_changed",
  "tool_requested",
]);
export type SupervisorCliJobCanceledReason = z.infer<
  typeof supervisorCliJobCanceledReasonSchema
>;

export const supervisorCliJobRecordSchema = z.object({
  id: z.string().min(1),
  toolUseId: z.string().min(1),
  cli: cliKindSchema,
  description: z.string().min(1).optional(),
  instruction: z.string().min(1),
  launch: supervisorCliLaunchOptionsSchema,
  status: supervisorCliJobStatusSchema,
  startedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
  exitCode: z.number().int().optional(),
  stdout: z.string().optional(),
  stderr: z.string().optional(),
  stdoutTruncated: z.boolean().optional(),
  stderrTruncated: z.boolean().optional(),
  canceledReason: supervisorCliJobCanceledReasonSchema.optional(),
});

export type SupervisorCliJobRecord = z.infer<
  typeof supervisorCliJobRecordSchema
>;

export const groupDiscussionDepthSchema = z.enum([
  "first_pass",
  "cross_review",
  "deep_diverge",
]);
export type GroupDiscussionDepth = z.infer<typeof groupDiscussionDepthSchema>;

export const groupDiscussionStatusSchema = z.enum([
  "running",
  "completed",
  "partial",
  "failed",
  "cancelled",
]);
export type GroupDiscussionStatus = z.infer<typeof groupDiscussionStatusSchema>;

export const groupDiscussionParticipantStatusSchema = z.enum([
  "running",
  "completed",
  "failed",
  "timeout",
]);
export type GroupDiscussionParticipantStatus = z.infer<
  typeof groupDiscussionParticipantStatusSchema
>;

export const groupDiscussionRoundSchema = z.object({
  round: z.number().int().min(1).max(2),
  depth: groupDiscussionDepthSchema,
  prompt: z.string().min(1),
  rawOutput: z.string().optional(),
  rawOutputRef: z.string().min(1).optional(),
  status: groupDiscussionParticipantStatusSchema,
  startedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
  error: z.string().min(1).optional(),
});
export type GroupDiscussionRound = z.infer<typeof groupDiscussionRoundSchema>;

export const groupDiscussionParticipantRecordSchema = z.object({
  participantId: z.string().min(1),
  modelId: z.string().min(1),
  providerId: z.string().min(1),
  modelAlias: routeModelAliasSchema,
  displayName: z.string().min(1),
  status: groupDiscussionParticipantStatusSchema,
  startedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
  stance: z.string().min(1).optional(),
  keyPoints: z.array(z.string().min(1)).default([]),
  risks: z.array(z.string().min(1)).default([]),
  recommendations: z.array(z.string().min(1)).default([]),
  rawOutput: z.string().optional(),
  rawOutputRef: z.string().min(1).optional(),
  error: z.string().min(1).optional(),
  rounds: z.array(groupDiscussionRoundSchema).default([]),
});
export type GroupDiscussionParticipantRecord = z.infer<
  typeof groupDiscussionParticipantRecordSchema
>;

export const groupDiscussionRecordSchema = z.object({
  id: z.string().min(1),
  toolUseId: z.string().min(1),
  topic: z.string().min(1),
  contextSnapshot: z.string().min(1),
  depth: groupDiscussionDepthSchema,
  status: groupDiscussionStatusSchema,
  participants: z.array(groupDiscussionParticipantRecordSchema),
  summary: z.string().min(1).optional(),
  consensus: z.array(z.string().min(1)).default([]),
  disagreements: z.array(z.string().min(1)).default([]),
  risks: z.array(z.string().min(1)).default([]),
  recommendations: z.array(z.string().min(1)).default([]),
  startedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
});
export type GroupDiscussionRecord = z.infer<typeof groupDiscussionRecordSchema>;

export const supervisorTranscriptEntrySchema = z.discriminatedUnion("kind", [
  z.object({
    id: z.string().min(1),
    kind: z.literal("user"),
    text: z.string().min(1),
    at: isoDateTimeSchema,
  }),
  z.object({
    id: z.string().min(1),
    kind: z.literal("assistant"),
    text: z.string().min(1),
    at: isoDateTimeSchema,
    status: z.enum(["streaming", "completed"]).default("completed"),
    startedAt: isoDateTimeSchema.optional(),
    completedAt: isoDateTimeSchema.optional(),
  }),
  z.object({
    id: z.string().min(1),
    kind: z.literal("thought"),
    title: z.string().trim().min(1).optional(),
    text: z.string().min(1),
    at: isoDateTimeSchema,
    status: z.enum(["streaming", "completed"]).default("completed"),
    startedAt: isoDateTimeSchema.optional(),
    completedAt: isoDateTimeSchema.optional(),
  }),
  z.object({
    id: z.string().min(1),
    kind: z.literal("tool"),
    toolUseId: z.string().min(1),
    name: supervisorToolNameSchema,
    input: z.record(z.string(), z.unknown()),
    status: z.enum(["completed", "failed"]),
    output: z.string(),
    startedAt: isoDateTimeSchema,
    completedAt: isoDateTimeSchema,
  }),
]);

export type SupervisorTranscriptEntry = z.infer<
  typeof supervisorTranscriptEntrySchema
>;

export const supervisorRunLiveEventSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("run_updated"),
    sessionId: z.string().min(1),
    run: z.lazy(() => supervisorRunRecordSchema),
  }),
  z.object({
    type: z.literal("transcript_entry_updated"),
    sessionId: z.string().min(1),
    runId: z.string().min(1),
    entry: supervisorTranscriptEntrySchema,
    updatedAt: isoDateTimeSchema,
  }),
]);

export type SupervisorRunLiveEvent = z.infer<
  typeof supervisorRunLiveEventSchema
>;

export const supervisorModelOverrideSchema = z.object({
  providerId: z.string().trim().min(1).optional(),
  modelAlias: routeModelAliasSchema.optional(),
});

export type SupervisorModelOverride = z.infer<
  typeof supervisorModelOverrideSchema
>;

export const supervisorRunStatusSchema = z.enum([
  "queued",
  "running",
  "completed",
  "canceled",
  "failed",
]);

export type SupervisorRunStatus = z.infer<typeof supervisorRunStatusSchema>;

export const supervisorRunDeliveryIntentSchema = z.enum(["queue", "interrupt"]);
export type SupervisorRunDeliveryIntent = z.infer<
  typeof supervisorRunDeliveryIntentSchema
>;

export const startSupervisorRunRequestSchema = z
  .object({
    projectId: z.string().trim().min(1),
    sessionId: z.string().trim().min(1).optional(),
    task: z.string().trim().min(1).max(supervisorRunTaskMaxChars),
    image: z.string().trim().min(1).default("janus-cli-worker:dev"),
    supervisorModel: supervisorModelOverrideSchema.optional(),
    deliveryIntent: supervisorRunDeliveryIntentSchema.default("queue"),
    discussionModelIds: z
      .array(z.string().trim().min(1))
      .min(1)
      .max(5)
      .optional(),
    attachments: z
      .array(supervisorRunAttachmentInputSchema)
      .max(supervisorRunAttachmentMaxCount)
      .optional(),
    bestOfN: supervisorBestOfNRequestSchema.optional(),
  })
  .strict()
  .superRefine((value, ctx) => {
    const ids = value.discussionModelIds;
    if (ids === undefined) {
      return;
    }

    if (new Set(ids).size !== ids.length) {
      ctx.addIssue({
        code: "custom",
        path: ["discussionModelIds"],
        message: "Duplicate discussion model ids are not allowed.",
      });
    }
  });

export type StartSupervisorRunInput = z.input<
  typeof startSupervisorRunRequestSchema
>;

export type StartSupervisorRunRequest = z.output<
  typeof startSupervisorRunRequestSchema
>;

export const supervisorRunRecordSchema = z.object({
  id: z.string().min(1),
  projectId: z.string().min(1),
  sessionId: z.string().min(1),
  task: z.string().min(1),
  image: z.string().min(1).optional(),
  supervisorModel: supervisorModelOverrideSchema.optional(),
  deliveryIntent: supervisorRunDeliveryIntentSchema.optional(),
  discussionModelIds: z.array(z.string().min(1)).min(1).max(5).optional(),
  attachments: z.array(supervisorRunAttachmentRecordSchema).optional(),
  bestOfN: supervisorBestOfNRunRecordSchema.optional(),
  askRequests: z.array(supervisorAskRequestRecordSchema).optional(),
  status: supervisorRunStatusSchema,
  transcript: z.array(supervisorTranscriptEntrySchema),
  cliJobs: z.array(supervisorCliJobRecordSchema).optional(),
  groupDiscussions: z.array(groupDiscussionRecordSchema).optional(),
  finalResponse: z.string().min(1).optional(),
  diff: sessionDiffSchema.optional(),
  deliveryRequestedAt: isoDateTimeSchema.optional(),
  deliveredToRunId: z.string().min(1).optional(),
  deliveredAt: isoDateTimeSchema.optional(),
  startedAt: isoDateTimeSchema,
  updatedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
  lastError: z.string().min(1).optional(),
});

export type SupervisorRunRecord = z.infer<typeof supervisorRunRecordSchema>;

export const startSupervisorRunResponseSchema = z.object({
  run: supervisorRunRecordSchema,
  diff: sessionDiffSchema.optional(),
});

export type StartSupervisorRunResponse = z.infer<
  typeof startSupervisorRunResponseSchema
>;

export const getSupervisorRunResponseSchema = z.object({
  run: supervisorRunRecordSchema,
});

export type GetSupervisorRunResponse = z.infer<
  typeof getSupervisorRunResponseSchema
>;

export const deliverQueuedSupervisorRunResponseSchema = z.object({
  run: supervisorRunRecordSchema,
});

export type DeliverQueuedSupervisorRunResponse = z.infer<
  typeof deliverQueuedSupervisorRunResponseSchema
>;

export const listSupervisorRunsResponseSchema = z.object({
  runs: z.array(supervisorRunRecordSchema),
});

export type ListSupervisorRunsResponse = z.infer<
  typeof listSupervisorRunsResponseSchema
>;

export const resolveSupervisorAskResponseSchema = z.object({
  run: supervisorRunRecordSchema,
});
export type ResolveSupervisorAskResponse = z.infer<
  typeof resolveSupervisorAskResponseSchema
>;
