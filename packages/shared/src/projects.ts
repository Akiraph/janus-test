import { z } from "zod";
import { isoDateTimeSchema } from "./common";
import { credentialAliasSchema } from "./credentials";
import {
  githubOwnerSchema,
  githubRepoNameSchema,
  repoProviderSchema,
} from "./repositories";

export const projectStatusSchema = z.enum(["ready", "clone_failed"]);
export type ProjectStatus = z.infer<typeof projectStatusSchema>;

export const connectProjectRequestSchema = z.object({
  provider: repoProviderSchema.default("github"),
  owner: githubOwnerSchema,
  repo: githubRepoNameSchema,
  gitCredentialAlias: credentialAliasSchema,
});

export type ConnectProjectInput = z.input<typeof connectProjectRequestSchema>;
export type ConnectProjectRequest = z.output<
  typeof connectProjectRequestSchema
>;

export const projectRecordSchema = z.object({
  id: z.string().min(1),
  provider: repoProviderSchema,
  owner: githubOwnerSchema,
  repo: githubRepoNameSchema,
  repoSlug: z.string().min(1),
  gitCredentialAlias: credentialAliasSchema,
  workspacePath: z.string().min(1),
  status: projectStatusSchema,
  createdAt: isoDateTimeSchema,
  updatedAt: isoDateTimeSchema,
});

export type ProjectRecord = z.infer<typeof projectRecordSchema>;

export const projectSummarySchema = projectRecordSchema.omit({
  gitCredentialAlias: true,
  workspacePath: true,
});

export type ProjectSummary = z.infer<typeof projectSummarySchema>;

export const connectProjectResponseSchema = z.object({
  project: projectSummarySchema,
});

export type ConnectProjectResponse = z.infer<
  typeof connectProjectResponseSchema
>;

export const listProjectsResponseSchema = z.object({
  projects: z.array(projectSummarySchema),
});

export type ListProjectsResponse = z.infer<typeof listProjectsResponseSchema>;
