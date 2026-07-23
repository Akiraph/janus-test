import { z } from "zod";
import { isoDateTimeSchema } from "./common";

export const credentialAliasSchema = z
  .string()
  .trim()
  .min(1)
  .max(80)
  .regex(/^[a-zA-Z0-9_.-]+$/, {
    message:
      "Alias may contain only letters, numbers, dots, underscores, and dashes.",
  });

export type CredentialAlias = z.infer<typeof credentialAliasSchema>;

export const credentialKinds = ["github_pat", "llm_api_key"] as const;
export const credentialKindSchema = z.enum(credentialKinds);
export type CredentialKind = z.infer<typeof credentialKindSchema>;

export const configureCredentialRequestSchema = z.object({
  alias: credentialAliasSchema,
  kind: credentialKindSchema,
  secret: z.string().min(1),
});

export type ConfigureCredentialRequest = z.infer<
  typeof configureCredentialRequestSchema
>;

export const credentialRecordSchema = z.object({
  alias: credentialAliasSchema,
  kind: credentialKindSchema,
  status: z.literal("stored"),
  updatedAt: isoDateTimeSchema,
  secretPreview: z.string().min(1).optional(),
});

export type CredentialRecord = z.infer<typeof credentialRecordSchema>;

export const configureCredentialResponseSchema = z.object({
  credential: credentialRecordSchema,
});

export type ConfigureCredentialResponse = z.infer<
  typeof configureCredentialResponseSchema
>;

export const listCredentialsResponseSchema = z.object({
  credentials: z.array(credentialRecordSchema),
});

export type ListCredentialsResponse = z.infer<
  typeof listCredentialsResponseSchema
>;
