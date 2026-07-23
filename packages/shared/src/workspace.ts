import { z } from "zod";
import { cliKindSchema } from "./cli";
import { isoDateTimeSchema } from "./common";
import { diffFileStatusSchema } from "./sessions";
import { supervisorRunStatusSchema } from "./supervisor";

/**
 * A conversation thread for the workspace left panel. A thread is one session;
 * its title belongs to the session, while status is derived from the latest
 * supervisor run on it.
 */
export const threadStatusSchema = z.union([
  supervisorRunStatusSchema,
  z.literal("idle"),
]);
export type ThreadStatus = z.infer<typeof threadStatusSchema>;

export const threadSummarySchema = z.object({
  sessionId: z.string().min(1),
  projectId: z.string().min(1),
  cli: cliKindSchema,
  title: z.string().min(1),
  status: threadStatusSchema,
  runCount: z.number().int().nonnegative(),
  updatedAt: isoDateTimeSchema,
});
export type ThreadSummary = z.infer<typeof threadSummarySchema>;

export const listProjectThreadsResponseSchema = z.object({
  threads: z.array(threadSummarySchema),
});
export type ListProjectThreadsResponse = z.infer<
  typeof listProjectThreadsResponseSchema
>;

/** A single entry when listing one directory level of the workspace tree. */
export const workspaceTreeEntrySchema = z.object({
  name: z.string().min(1),
  path: z.string().min(1),
  type: z.enum(["dir", "file"]),
  changed: diffFileStatusSchema.optional(),
});
export type WorkspaceTreeEntry = z.infer<typeof workspaceTreeEntrySchema>;

export const workspaceTreeResponseSchema = z.object({
  path: z.string(),
  entries: z.array(workspaceTreeEntrySchema),
});
export type WorkspaceTreeResponse = z.infer<typeof workspaceTreeResponseSchema>;

export const workspaceFileSchema = z.object({
  path: z.string().min(1),
  content: z.string(),
  encoding: z.literal("utf-8"),
  truncated: z.boolean(),
});
export type WorkspaceFile = z.infer<typeof workspaceFileSchema>;

export const workspaceFileResponseSchema = z.object({
  file: workspaceFileSchema,
});
export type WorkspaceFileResponse = z.infer<typeof workspaceFileResponseSchema>;

/** Request to overwrite a workspace file with new utf-8 contents. */
export const writeWorkspaceFileRequestSchema = z.object({
  path: z.string().min(1),
  content: z.string(),
});
export type WriteWorkspaceFileRequest = z.infer<
  typeof writeWorkspaceFileRequestSchema
>;

export const writeWorkspaceFileResponseSchema = z.object({
  file: workspaceFileSchema,
});
export type WriteWorkspaceFileResponse = z.infer<
  typeof writeWorkspaceFileResponseSchema
>;

export const renameWorkspacePathRequestSchema = z.object({
  fromPath: z.string().min(1),
  toPath: z.string().min(1),
});
export type RenameWorkspacePathRequest = z.infer<
  typeof renameWorkspacePathRequestSchema
>;

export const renameWorkspacePathResponseSchema = z.object({
  entry: workspaceTreeEntrySchema,
});
export type RenameWorkspacePathResponse = z.infer<
  typeof renameWorkspacePathResponseSchema
>;

export const deleteWorkspacePathRequestSchema = z.object({
  path: z.string().min(1),
});
export type DeleteWorkspacePathRequest = z.infer<
  typeof deleteWorkspacePathRequestSchema
>;

export const deleteWorkspacePathResponseSchema = z.object({
  path: z.string().min(1),
});
export type DeleteWorkspacePathResponse = z.infer<
  typeof deleteWorkspacePathResponseSchema
>;

export const gitCommitSchema = z.object({
  sha: z.string().min(1),
  parentShas: z.array(z.string().min(1)).optional(),
  message: z.string(),
  author: z.string(),
  at: isoDateTimeSchema,
  byJanus: z.boolean(),
});
export type GitCommit = z.infer<typeof gitCommitSchema>;

export const gitHistoryResponseSchema = z.object({
  branches: z.array(z.string().min(1)),
  commits: z.array(gitCommitSchema),
});
export type GitHistoryResponse = z.infer<typeof gitHistoryResponseSchema>;

export const gitFileChangeSchema = z.object({
  path: z.string().min(1),
  originalPath: z.string().min(1).optional(),
  status: diffFileStatusSchema,
});
export type GitFileChange = z.infer<typeof gitFileChangeSchema>;

export const gitStatusResponseSchema = z.object({
  branch: z.string().min(1),
  stagedChanges: z.array(gitFileChangeSchema),
  changes: z.array(gitFileChangeSchema),
  clean: z.boolean(),
});
export type GitStatusResponse = z.infer<typeof gitStatusResponseSchema>;

export const gitPathRequestSchema = z.object({
  path: z.string().min(1),
});
export type GitPathRequest = z.infer<typeof gitPathRequestSchema>;

export const commitGitChangesRequestSchema = z.object({
  message: z.string().trim().min(1),
});
export type CommitGitChangesRequest = z.infer<
  typeof commitGitChangesRequestSchema
>;

export const commitGitChangesResponseSchema = z.object({
  commit: gitCommitSchema,
  status: gitStatusResponseSchema,
});
export type CommitGitChangesResponse = z.infer<
  typeof commitGitChangesResponseSchema
>;
