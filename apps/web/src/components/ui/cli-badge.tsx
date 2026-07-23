import type { CliKind } from "@janus/shared";
import { Badge } from "./badge";

const CLI_META: Record<CliKind, { label: string; tone: "cc" | "cx" }> = {
  "claude-code": { label: "Claude Code", tone: "cc" },
  codex: { label: "Codex", tone: "cx" },
};

export interface CliBadgeProps {
  readonly cli: CliKind;
  readonly short?: boolean;
}

export function CliBadge({ cli, short = false }: CliBadgeProps) {
  const meta = CLI_META[cli];
  return <Badge tone={meta.tone}>{short ? shortLabel(cli) : meta.label}</Badge>;
}

function shortLabel(cli: CliKind): string {
  return cli === "claude-code" ? "cc" : "cx";
}
