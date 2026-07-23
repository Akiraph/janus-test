import { z } from "zod";
import { cliKindSchema } from "./cli";
import { isoDateTimeSchema } from "./common";
import { credentialAliasSchema } from "./credentials";

export const sessionStatusSchema = z.enum([
  "starting",
  "running",
  "dispatching",
  "completed",
  "canceled",
  "failed",
]);
export type SessionStatus = z.infer<typeof sessionStatusSchema>;

export const createSessionRequestSchema = z.object({
  projectId: z.string().trim().min(1),
  // Optional: a session can be created before any LLM credential is configured.
  // The sandbox is only started when a credential is available.
  llmCredentialAlias: credentialAliasSchema.optional(),
  image: z.string().trim().min(1).default("janus-cli-worker:dev"),
});

export type CreateSessionInput = z.input<typeof createSessionRequestSchema>;
export type CreateSessionRequest = z.output<typeof createSessionRequestSchema>;

export const sessionTitleSchema = z.string().trim().min(1).max(120);

export const sessionRecordSchema = z.object({
  id: z.string().min(1),
  projectId: z.string().min(1),
  title: sessionTitleSchema.optional(),
  cli: cliKindSchema,
  status: sessionStatusSchema,
  sandboxId: z.string().min(1).optional(),
  workspacePath: z.string().min(1).optional(),
  workspaceBaseRef: z.string().min(1).optional(),
  // Optional: a session can be created before any LLM credential is configured.
  // The sandbox is only started when a credential is available.
  llmCredentialAlias: credentialAliasSchema.optional(),
  modelGatewayUrl: z.string().min(1),
  startedAt: isoDateTimeSchema,
  completedAt: isoDateTimeSchema.optional(),
  lastError: z.string().min(1).optional(),
});

export type SessionRecord = z.infer<typeof sessionRecordSchema>;

export const createSessionResponseSchema = z.object({
  session: sessionRecordSchema,
});

export type CreateSessionResponse = z.infer<typeof createSessionResponseSchema>;

export const renameSessionRequestSchema = z
  .object({
    title: sessionTitleSchema,
  })
  .strict();
export type RenameSessionRequest = z.infer<typeof renameSessionRequestSchema>;

export const renameSessionResponseSchema = z.object({
  session: sessionRecordSchema,
});
export type RenameSessionResponse = z.infer<typeof renameSessionResponseSchema>;

export const dispatchInstructionRequestSchema = z.object({
  instruction: z.string().trim().min(1),
});

export type DispatchInstructionRequest = z.infer<
  typeof dispatchInstructionRequestSchema
>;

export const activityEventTypes = [
  "session_created",
  "session_renamed",
  "sandbox_started",
  "cli_dispatched",
  "cli_output",
  "discussion_started",
  "discussion_participant_started",
  "discussion_participant_completed",
  "discussion_round_completed",
  "discussion_completed",
  "discussion_partial",
  "discussion_failed",
  "discussion_cancelled",
  "supervisor_model_retry",
  "supervisor_model_recovered",
  "supervisor_model_warning",
  "diff_recorded",
  "checkpoint_recorded",
  "session_completed",
  "session_canceled",
  "session_failed",
] as const;
export const activityEventTypeSchema = z.enum(activityEventTypes);
export type ActivityEventType = z.infer<typeof activityEventTypeSchema>;

export const activityEventLevels = ["info", "warn", "error"] as const;
export const activityEventLevelSchema = z.enum(activityEventLevels);
export type ActivityEventLevel = z.infer<typeof activityEventLevelSchema>;

export const activityEventSchema = z.object({
  id: z.string().min(1),
  sessionId: z.string().min(1),
  sequence: z.number().int().nonnegative(),
  type: activityEventTypeSchema,
  level: activityEventLevelSchema,
  message: z.string().min(1),
  timestamp: isoDateTimeSchema,
});

export type ActivityEvent = z.infer<typeof activityEventSchema>;

export const sessionActivityResponseSchema = z.object({
  events: z.array(activityEventSchema),
});

export type SessionActivityResponse = z.infer<
  typeof sessionActivityResponseSchema
>;

export const diffFileStatusSchema = z.enum([
  "added",
  "deleted",
  "modified",
  "renamed",
  "untracked",
]);

export type DiffFileStatus = z.infer<typeof diffFileStatusSchema>;

export const sessionDiffFileSchema = z.object({
  path: z.string().min(1),
  status: diffFileStatusSchema,
  additions: z.number().int().nonnegative(),
  deletions: z.number().int().nonnegative(),
});

export type SessionDiffFile = z.infer<typeof sessionDiffFileSchema>;

export const sessionDiffSchema = z.object({
  sessionId: z.string().min(1),
  files: z.array(sessionDiffFileSchema),
  patch: z.string(),
  updatedAt: isoDateTimeSchema,
});

export type SessionDiff = z.infer<typeof sessionDiffSchema>;

export const sessionCheckpointOriginSchema = z.enum([
  "run",
  "branch",
  "rewind",
]);
export type SessionCheckpointOrigin = z.infer<
  typeof sessionCheckpointOriginSchema
>;

export const sessionCheckpointSnapshotSchema = z.object({
  version: z.literal(1),
  conversation: z.object({
    runCount: z.number().int().nonnegative(),
    transcriptEntryCount: z.number().int().nonnegative(),
    recentEntries: z
      .array(
        z.object({
          runId: z.string().min(1),
          kind: z.enum(["user", "assistant", "thought"]),
          text: z.string().min(1).max(1000),
          at: isoDateTimeSchema,
          title: z.string().min(1).max(120).optional(),
        }),
      )
      .max(20),
  }),
  todo: z
    .object({
      sourceRunId: z.string().min(1),
      updatedAt: isoDateTimeSchema,
      items: z
        .array(
          z.object({
            content: z.string().min(1).max(500),
            status: z.enum(["pending", "in_progress", "completed"]),
          }),
        )
        .max(20),
    })
    .optional(),
  tools: z.object({
    toolCallCount: z.number().int().nonnegative(),
    latestToolCalls: z
      .array(
        z.object({
          runId: z.string().min(1),
          name: z.string().min(1).max(80),
          status: z.enum(["completed", "failed"]),
          completedAt: isoDateTimeSchema,
          outputSummary: z.string().max(1000).optional(),
        }),
      )
      .max(20),
    cliJobs: z.object({
      running: z.number().int().nonnegative(),
      completed: z.number().int().nonnegative(),
      failed: z.number().int().nonnegative(),
      canceled: z.number().int().nonnegative(),
    }),
    groupDiscussions: z.object({
      running: z.number().int().nonnegative(),
      completed: z.number().int().nonnegative(),
      partial: z.number().int().nonnegative(),
      failed: z.number().int().nonnegative(),
      cancelled: z.number().int().nonnegative(),
    }),
  }),
  runtime: z.object({
    sessionStatus: sessionStatusSchema,
    runStatus: z.string().min(1).max(80),
    workspaceBaseRef: z.string().min(1).max(500).optional(),
    hasSessionWorkspace: z.boolean(),
    completedAt: isoDateTimeSchema.optional(),
  }),
});
export type SessionCheckpointSnapshot = z.infer<
  typeof sessionCheckpointSnapshotSchema
>;

export const sessionCheckpointRecordSchema = z.object({
  id: z.string().min(1),
  projectId: z.string().min(1),
  sessionId: z.string().min(1),
  runId: z.string().min(1).optional(),
  parentCheckpointId: z.string().min(1).optional(),
  ref: z.string().min(1),
  commitSha: z.string().min(1),
  title: z.string().min(1),
  origin: sessionCheckpointOriginSchema,
  createdAt: isoDateTimeSchema,
  snapshot: sessionCheckpointSnapshotSchema.optional(),
});
export type SessionCheckpointRecord = z.infer<
  typeof sessionCheckpointRecordSchema
>;

export const listSessionCheckpointsResponseSchema = z.object({
  checkpoints: z.array(sessionCheckpointRecordSchema),
});
export type ListSessionCheckpointsResponse = z.infer<
  typeof listSessionCheckpointsResponseSchema
>;

export const branchSessionRequestSchema = z
  .object({
    checkpointId: z.string().trim().min(1).optional(),
    runId: z.string().trim().min(1).optional(),
    origin: z.enum(["branch", "rewind"]).optional(),
  })
  .strict();
export type BranchSessionRequest = z.infer<typeof branchSessionRequestSchema>;

export const branchSessionResponseSchema = z.object({
  session: sessionRecordSchema,
  checkpoint: sessionCheckpointRecordSchema.optional(),
  copiedRunCount: z.number().int().nonnegative(),
});
export type BranchSessionResponse = z.infer<typeof branchSessionResponseSchema>;

export const sessionApplyStatusSchema = z.enum([
  "integrating",
  "review_ready",
  "conflicted",
  "resolving",
  "applied",
  "failed",
]);
export type SessionApplyStatus = z.infer<typeof sessionApplyStatusSchema>;

export const sessionApplyConflictResolutionStatusSchema = z.enum([
  "not_required",
  "needed",
  "started",
  "completed",
  "failed",
]);
export type SessionApplyConflictResolutionStatus = z.infer<
  typeof sessionApplyConflictResolutionStatusSchema
>;

export const startSessionApplyRequestSchema = z
  .object({
    checkpointId: z.string().trim().min(1).optional(),
    runId: z.string().trim().min(1).optional(),
  })
  .strict();
export type StartSessionApplyRequest = z.infer<
  typeof startSessionApplyRequestSchema
>;

export const sessionApplySourceSchema = z.object({
  checkpointId: z.string().min(1),
  runId: z.string().min(1).optional(),
  ref: z.string().min(1),
  commitSha: z.string().min(1),
  title: z.string().min(1),
});
export type SessionApplySource = z.infer<typeof sessionApplySourceSchema>;

export const sessionApplyIntegrationSchema = z.object({
  workspacePath: z.string().min(1),
  mainRef: z.string().min(1),
  mainCommitSha: z.string().min(1),
  baseRef: z.string().min(1).optional(),
  createdAt: isoDateTimeSchema,
  updatedAt: isoDateTimeSchema,
});
export type SessionApplyIntegration = z.infer<
  typeof sessionApplyIntegrationSchema
>;

export const sessionApplyReviewSchema = z.object({
  diff: sessionDiffSchema.optional(),
  conflictedFiles: z.array(z.string().min(1)),
});
export type SessionApplyReview = z.infer<typeof sessionApplyReviewSchema>;

export const sessionApplyConflictResolutionSchema = z.object({
  status: sessionApplyConflictResolutionStatusSchema,
  sessionId: z.string().min(1).optional(),
  runId: z.string().min(1).optional(),
  prompt: z.string().min(1).optional(),
  startedAt: isoDateTimeSchema.optional(),
  completedAt: isoDateTimeSchema.optional(),
});
export type SessionApplyConflictResolution = z.infer<
  typeof sessionApplyConflictResolutionSchema
>;

export const sessionApplyRecordSchema = z.object({
  id: z.string().min(1),
  projectId: z.string().min(1),
  sessionId: z.string().min(1),
  status: sessionApplyStatusSchema,
  source: sessionApplySourceSchema,
  integration: sessionApplyIntegrationSchema,
  review: sessionApplyReviewSchema,
  conflictResolution: sessionApplyConflictResolutionSchema,
  createdAt: isoDateTimeSchema,
  updatedAt: isoDateTimeSchema,
  appliedAt: isoDateTimeSchema.optional(),
  error: z.string().min(1).optional(),
});
export type SessionApplyRecord = z.infer<typeof sessionApplyRecordSchema>;

export const sessionApplyRecordResponseSchema = z.object({
  apply: sessionApplyRecordSchema,
});
export type SessionApplyRecordResponse = z.infer<
  typeof sessionApplyRecordResponseSchema
>;

export const listSessionApplyRecordsResponseSchema = z.object({
  applies: z.array(sessionApplyRecordSchema),
});
export type ListSessionApplyRecordsResponse = z.infer<
  typeof listSessionApplyRecordsResponseSchema
>;

export const dispatchInstructionResponseSchema = z.object({
  session: sessionRecordSchema,
  diff: sessionDiffSchema,
});

export type DispatchInstructionResponse = z.infer<
  typeof dispatchInstructionResponseSchema
>;
