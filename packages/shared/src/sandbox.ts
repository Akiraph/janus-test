import { z } from "zod";

export const sandboxResourceLimitsSchema = z.object({
  cpus: z.number().positive().max(4),
  memoryMb: z.number().int().min(128).max(8192),
  pidsLimit: z.number().int().min(32).max(1024),
});

export type SandboxResourceLimits = z.infer<typeof sandboxResourceLimitsSchema>;

export const startEmptySandboxRequestSchema = z.object({
  sessionId: z.string().trim().min(1),
  image: z.string().trim().min(1).default("janus-cli-worker:dev"),
  resourceLimits: sandboxResourceLimitsSchema.optional(),
});

export type StartEmptySandboxInput = z.input<
  typeof startEmptySandboxRequestSchema
>;

export type StartEmptySandboxRequest = z.output<
  typeof startEmptySandboxRequestSchema
>;

export const sandboxHardeningPolicySchema = z.object({
  user: z.literal("10001:10001"),
  readOnlyRootFilesystem: z.literal(true),
  noHostDockerSocket: z.literal(true),
  networkMode: z.string().min(1),
  egressAllowlist: z.array(z.string().min(1)).optional(),
  capabilityDrop: z.array(z.literal("ALL")),
  securityOptions: z.array(z.literal("no-new-privileges")),
  tmpfs: z.array(z.string().min(1)),
  resourceLimits: sandboxResourceLimitsSchema,
});

export type SandboxHardeningPolicy = z.infer<
  typeof sandboxHardeningPolicySchema
>;

export const startEmptySandboxResponseSchema = z.object({
  sandboxId: z.string().min(1),
  status: z.literal("template_ready"),
  image: z.string().min(1),
  hardening: sandboxHardeningPolicySchema,
});

export type StartEmptySandboxResponse = z.infer<
  typeof startEmptySandboxResponseSchema
>;
