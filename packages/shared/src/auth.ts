import { z } from "zod";
import { isoDateTimeSchema } from "./common";

export const authSessionTtlSeconds = 7 * 24 * 60 * 60;

export const ownerUsernameSchema = z
  .string()
  .trim()
  .min(3)
  .max(64)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._-]*$/);

export const ownerPasswordSchema = z.string().min(12).max(256);

export const authOwnerRecordSchema = z.object({
  id: z.literal("local-owner"),
  username: ownerUsernameSchema,
  passwordHash: z.string().min(1),
  requiresCredentialSetup: z.boolean(),
  createdAt: isoDateTimeSchema,
  updatedAt: isoDateTimeSchema,
  bootstrapCredentialFilePath: z.string().min(1).optional(),
});

export type AuthOwnerRecord = z.infer<typeof authOwnerRecordSchema>;

export const authSessionRecordSchema = z.object({
  id: z.string().min(1),
  tokenHash: z.string().min(1),
  issuedAt: isoDateTimeSchema,
  expiresAt: isoDateTimeSchema.optional(),
  revokedAt: isoDateTimeSchema.optional(),
});

export type AuthSessionRecord = z.infer<typeof authSessionRecordSchema>;

export const authUserSchema = z.object({
  username: ownerUsernameSchema,
  requiresCredentialSetup: z.boolean(),
});

export type AuthUser = z.infer<typeof authUserSchema>;

export const authStatusResponseSchema = z.object({
  authenticated: z.boolean(),
  user: authUserSchema.optional(),
});

export type AuthStatusResponse = z.infer<typeof authStatusResponseSchema>;

export const loginRequestSchema = z.object({
  username: ownerUsernameSchema,
  password: z.string().min(1).max(256),
});

export type LoginRequest = z.infer<typeof loginRequestSchema>;

export const updateOwnerCredentialsRequestSchema = z.object({
  currentPassword: z.string().min(1).max(256),
  username: ownerUsernameSchema,
  password: ownerPasswordSchema,
});

export type UpdateOwnerCredentialsRequest = z.infer<
  typeof updateOwnerCredentialsRequestSchema
>;

export const updateOwnerUsernameRequestSchema = z.object({
  username: ownerUsernameSchema,
});

export type UpdateOwnerUsernameRequest = z.infer<
  typeof updateOwnerUsernameRequestSchema
>;

export const updateOwnerPasswordRequestSchema = z.object({
  currentPassword: z.string().min(1).max(256),
  password: ownerPasswordSchema,
});

export type UpdateOwnerPasswordRequest = z.infer<
  typeof updateOwnerPasswordRequestSchema
>;
