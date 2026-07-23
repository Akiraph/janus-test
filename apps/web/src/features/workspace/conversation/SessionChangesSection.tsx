import type {
  SessionDiff,
  SessionDiffFile,
  SupervisorRunRecord,
} from "@janus/shared";
import { FileDiff, Loader2 } from "lucide-react";
import { Button } from "../../../components/ui/button";
import { DiffView } from "../evidence-views";
import { useSupervisorRuns } from "../hooks/useSupervisorRuns";
import { RailEmptyState } from "./SessionRightRail";

const STATUS_LABELS: Record<SessionDiffFile["status"], string> = {
  added: "A",
  deleted: "D",
  modified: "M",
  renamed: "R",
  untracked: "U",
};

export function latestRunDiff(
  runs: readonly SupervisorRunRecord[],
): SessionDiff | undefined {
  return runs
    .map((run) => run.diff)
    .filter((diff): diff is SessionDiff => diff !== undefined)
    .filter((diff) => diff.files.length > 0 || diff.patch.trim().length > 0)
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))[0];
}

export function diffTotals(diff: SessionDiff | undefined): {
  readonly files: number;
  readonly additions: number;
  readonly deletions: number;
} {
  return (diff?.files ?? []).reduce(
    (totals, file) => ({
      files: totals.files + 1,
      additions: totals.additions + file.additions,
      deletions: totals.deletions + file.deletions,
    }),
    { files: 0, additions: 0, deletions: 0 },
  );
}

export function SessionChangesSection({
  diff,
  loading,
  onOpenDiff,
}: {
  readonly diff: SessionDiff | undefined;
  readonly loading: boolean;
  readonly onOpenDiff: () => void;
}) {
  const totals = diffTotals(diff);

  if (diff === undefined) {
    return (
      <RailEmptyState>
        {loading ? "Loading file changes..." : "No file changes yet."}
      </RailEmptyState>
    );
  }

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-3 gap-2 text-2xs text-muted-foreground">
        <Meta label="Files" value={String(totals.files)} />
        <Meta label="Added" value={`+${totals.additions}`} />
        <Meta label="Deleted" value={`-${totals.deletions}`} />
      </div>
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={onOpenDiff}
        className="w-full"
      >
        <FileDiff className="h-3.5 w-3.5" />
        Open diff
      </Button>
      <ul className="overflow-hidden rounded-md border border-border bg-card">
        {diff.files.map((file) => (
          <li
            key={file.path}
            className="flex min-w-0 items-center gap-2 border-b border-border px-2.5 py-1.5 last:border-b-0"
          >
            <span className="grid h-4 w-4 shrink-0 place-items-center rounded-xs font-mono text-[10px] font-bold text-faint">
              {STATUS_LABELS[file.status]}
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
              {file.path}
            </span>
            <span className="shrink-0 font-mono text-[11px]">
              <span className="text-success">+{file.additions}</span>{" "}
              <span className="text-destructive">-{file.deletions}</span>
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function SessionDiffPanel({
  sessionId,
}: {
  readonly sessionId: string;
}) {
  const runsQuery = useSupervisorRuns(sessionId);
  const diff = latestRunDiff(runsQuery.data?.runs ?? []);

  if (diff === undefined) {
    return (
      <div className="flex h-full items-center justify-center bg-background p-6">
        <RailEmptyState>
          {runsQuery.isLoading ? (
            <span className="inline-flex items-center gap-2">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              Loading file changes
            </span>
          ) : (
            "No file changes yet."
          )}
        </RailEmptyState>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto bg-background p-4">
      <DiffView diff={diff} />
    </div>
  );
}

function Meta({
  label,
  value,
}: {
  readonly label: string;
  readonly value: string;
}) {
  return (
    <div className="min-w-0 rounded-sm bg-muted px-2 py-1">
      <div className="text-faint">{label}</div>
      <div className="truncate font-mono text-muted-foreground">{value}</div>
    </div>
  );
}
