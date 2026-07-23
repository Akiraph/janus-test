import { z } from "zod";

export const cliKinds = ["claude-code", "codex"] as const;
export const cliKindSchema = z.enum(cliKinds);
export type CliKind = z.infer<typeof cliKindSchema>;
