import { z } from "zod";
import { isoDateTimeSchema } from "./common";
import { supervisorRunRecordSchema } from "./supervisor";
import { threadSummarySchema } from "./workspace";

export const projectThreadChangeReasonSchema = z.enum([
  "session_created",
  "session_renamed",
  "session_deleted",
  "session_completed",
  "session_canceled",
  "session_failed",
  "sandbox_started",
  "run_updated",
  "branch_created",
]);
export type ProjectThreadChangeReason = z.infer<
  typeof projectThreadChangeReasonSchema
>;

export const projectThreadsLiveEventSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("threads_snapshot"),
    projectId: z.string().min(1),
    threads: z.array(threadSummarySchema),
    emittedAt: isoDateTimeSchema,
  }),
  z.object({
    type: z.literal("threads_changed"),
    projectId: z.string().min(1),
    reason: projectThreadChangeReasonSchema,
    sessionId: z.string().min(1).optional(),
    run: supervisorRunRecordSchema.optional(),
    emittedAt: isoDateTimeSchema,
  }),
]);
export type ProjectThreadsLiveEvent = z.infer<
  typeof projectThreadsLiveEventSchema
>;
