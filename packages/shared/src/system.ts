import { z } from "zod";
import { isoDateTimeSchema } from "./common";

export const healthResponseSchema = z.object({
  service: z.literal("janus-server"),
  status: z.literal("ok"),
  version: z.string(),
  time: isoDateTimeSchema,
});

export type HealthResponse = z.infer<typeof healthResponseSchema>;

export const readinessDependencyStatusSchema = z.enum(["ok", "degraded"]);

export const readinessResponseSchema = z.object({
  service: z.literal("janus-server"),
  status: readinessDependencyStatusSchema,
  version: z.string(),
  time: isoDateTimeSchema,
  dependencies: z.array(
    z.object({
      name: z.string().min(1),
      status: readinessDependencyStatusSchema,
      message: z.string().min(1).optional(),
    }),
  ),
});

export type ReadinessResponse = z.infer<typeof readinessResponseSchema>;
