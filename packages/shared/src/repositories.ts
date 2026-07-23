import { z } from "zod";
import { isoDateTimeSchema } from "./common";

export const repoProviders = ["github"] as const;
export const repoProviderSchema = z.enum(repoProviders);
export type RepoProvider = z.infer<typeof repoProviderSchema>;

export const githubOwnerSchema = z
  .string()
  .trim()
  .min(1)
  .max(39)
  .regex(/^[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?$/, {
    message:
      "GitHub owner may contain only letters, numbers, and internal dashes.",
  })
  .refine((value) => !value.includes("--"), {
    message: "GitHub owner may not contain consecutive dashes.",
  });

export type GitHubOwner = z.infer<typeof githubOwnerSchema>;

export const githubRepoNameSchema = z
  .string()
  .trim()
  .min(1)
  .max(100)
  .regex(/^[a-zA-Z0-9._-]+$/, {
    message:
      "GitHub repo may contain only letters, numbers, dots, underscores, and dashes.",
  })
  .refine((value) => value !== "." && value !== "..", {
    message: "GitHub repo may not be a path traversal segment.",
  });

export type GitHubRepoName = z.infer<typeof githubRepoNameSchema>;

export const repoAuthorizationModes = ["oauth", "pat", "github_app"] as const;
export const repoAuthorizationModeSchema = z.enum(repoAuthorizationModes);
export type RepoAuthorizationMode = z.infer<typeof repoAuthorizationModeSchema>;

export const repoAuthorizationRequestSchema = z.object({
  provider: repoProviderSchema.default("github"),
  owner: githubOwnerSchema,
  repo: githubRepoNameSchema,
  mode: repoAuthorizationModeSchema,
  tokenAlias: z.string().trim().min(1).optional(),
});

export type RepoAuthorizationInput = z.input<
  typeof repoAuthorizationRequestSchema
>;

export type RepoAuthorizationRequest = z.output<
  typeof repoAuthorizationRequestSchema
>;

export const repoAuthorizationRecordSchema = z.object({
  id: z.string().min(1),
  provider: repoProviderSchema,
  owner: githubOwnerSchema,
  repo: githubRepoNameSchema,
  repoSlug: z.string().min(1),
  mode: repoAuthorizationModeSchema,
  status: z.literal("authorized"),
  authorizedAt: isoDateTimeSchema,
  tokenAlias: z.string().min(1).optional(),
});

export type RepoAuthorizationRecord = z.infer<
  typeof repoAuthorizationRecordSchema
>;

export const repoAuthorizationResponseSchema = z.object({
  authorization: repoAuthorizationRecordSchema,
});

export type RepoAuthorizationResponse = z.infer<
  typeof repoAuthorizationResponseSchema
>;

export const pullRequestResultSchema = z.object({
  branchName: z.string().min(1),
  status: z.enum(["created"]),
  url: z.string().url(),
  number: z.number().int().positive(),
});

export type PullRequestResult = z.infer<typeof pullRequestResultSchema>;
