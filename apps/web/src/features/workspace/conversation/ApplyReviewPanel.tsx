import type { SessionApplyRecord } from "@janus/shared";
import {
  AlertTriangle,
  Check,
  GitPullRequest,
  Loader2,
  RefreshCw,
} from "lucide-react";
import { Button } from "../../../components/ui/button";
import { type DotTone, StatusDot } from "../../../components/ui/status-dot";
import { cn } from "../../../lib/cn";
import { DiffView, FileList } from "../evidence-views";
import {
  useApplySessionApplyReview,
  useRefreshSessionApplyReview,
  useSessionApplyRecords,
  useStartSessionApply,
} from "../hooks/useSessionApplyRecords";
import { RailEmptyState } from "./SessionRightRail";

interface ApplyReviewPanelProps {
  readonly sessionId: string;
  readonly projectId: string;
}

export function ApplyReviewPanel({
  sessionId,
  projectId,
}: ApplyReviewPanelProps) {
  const applyRecords = useSessionApplyRecords(sessionId);
  const startApply = useStartSessionApply(sessionId, projectId);
  const refreshApply = useRefreshSessionApplyReview(sessionId);
  const applyReview = useApplySessionApplyReview(sessionId, projectId);
  const latest = applyRecords.data?.applies[0];
  const busy =
    startApply.isPending || refreshApply.isPending || applyReview.isPending;
  const actionError =
    errorMessage(startApply.error) ??
    errorMessage(refreshApply.error) ??
    errorMessage(applyReview.error) ??
    errorMessage(applyRecords.error);

  return (
    <div className="space-y-3">
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => startApply.mutate({})}
        disabled={busy}
        className="w-full"
      >
        {startApply.isPending ? (
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
        ) : (
          <GitPullRequest className="h-3.5 w-3.5" />
        )}
        Prepare apply
      </Button>

      {actionError === undefined ? null : (
        <div className="rounded-sm border border-destructive/30 bg-destructive-soft px-2.5 py-2 text-xs text-destructive">
          {actionError}
        </div>
      )}

      {applyRecords.isLoading ? (
        <RailEmptyState>
          <span className="inline-flex items-center gap-2">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            Loading apply records
          </span>
        </RailEmptyState>
      ) : latest === undefined ? (
        <RailEmptyState>No apply review prepared.</RailEmptyState>
      ) : (
        <ApplyRecordReview
          apply={latest}
          busy={busy}
          refreshing={refreshApply.isPending}
          applying={applyReview.isPending}
          onRefresh={() => refreshApply.mutate(latest.id)}
          onApply={() => applyReview.mutate(latest.id)}
        />
      )}
    </div>
  );
}

function ApplyRecordReview({
  apply,
  busy,
  refreshing,
  applying,
  onRefresh,
  onApply,
}: {
  readonly apply: SessionApplyRecord;
  readonly busy: boolean;
  readonly refreshing: boolean;
  readonly applying: boolean;
  readonly onRefresh: () => void;
  readonly onApply: () => void;
}) {
  const canRefresh =
    apply.status === "resolving" || apply.status === "conflicted";
  const hasReviewChanges = reviewHasChanges(apply);
  const canApply = apply.status === "review_ready" && hasReviewChanges;
  const totals = reviewLineTotals(apply);

  return (
    <div className="space-y-3">
      <div className="rounded-md border border-border bg-card p-3">
        <div className="flex items-start gap-2">
          <StatusDot
            tone={statusTone(apply)}
            pulse={apply.status === "resolving"}
          />
          <div className="min-w-0 flex-1">
            <div className="text-xs font-medium text-foreground">
              {statusLabel(apply)}
            </div>
            <div className="mt-1 truncate font-mono text-2xs text-faint">
              {apply.source.title}
            </div>
          </div>
          {apply.status === "applied" ? (
            <Check className="h-4 w-4 shrink-0 text-success" aria-hidden />
          ) : apply.review.conflictedFiles.length > 0 ? (
            <AlertTriangle
              className="h-4 w-4 shrink-0 text-warning"
              aria-hidden
            />
          ) : null}
        </div>
        <div className="mt-3 grid grid-cols-2 gap-2 text-2xs text-muted-foreground">
          <Meta
            label="Files"
            value={String(apply.review.diff?.files.length ?? 0)}
          />
          <Meta label="Added" value={`+${totals.additions}`} />
          <Meta label="Deleted" value={`-${totals.deletions}`} />
          <Meta
            label="Conflicts"
            value={String(apply.review.conflictedFiles.length)}
          />
          <Meta
            label="Main"
            value={apply.integration.mainCommitSha.slice(0, 7)}
          />
          <Meta label="Source" value={apply.source.commitSha.slice(0, 7)} />
        </div>
      </div>

      {apply.error === undefined ? null : (
        <div className="rounded-sm border border-destructive/30 bg-destructive-soft px-2.5 py-2 text-xs text-destructive">
          {apply.error}
        </div>
      )}

      {apply.review.conflictedFiles.length === 0 ? null : (
        <div className="space-y-1.5">
          <div className="text-2xs font-semibold text-faint">Conflicts</div>
          <FileList paths={apply.review.conflictedFiles} />
        </div>
      )}

      {apply.review.diff === undefined ? null : (
        <div className="space-y-1.5">
          <div className="text-2xs font-semibold text-faint">Review diff</div>
          {hasReviewChanges ? (
            <DiffView diff={apply.review.diff} />
          ) : (
            <RailEmptyState>No changes to apply.</RailEmptyState>
          )}
        </div>
      )}

      <div className="flex gap-2">
        {canRefresh ? (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onRefresh}
            disabled={busy}
            className="flex-1"
          >
            {refreshing ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <RefreshCw className="h-3.5 w-3.5" />
            )}
            Refresh
          </Button>
        ) : null}
        <Button
          type="button"
          variant={canApply ? "success" : "outline"}
          size="sm"
          onClick={onApply}
          disabled={!canApply || busy}
          className={cn(canRefresh ? "flex-1" : "w-full")}
        >
          {applying ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Check className="h-3.5 w-3.5" />
          )}
          Apply
        </Button>
      </div>
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

function statusLabel(apply: SessionApplyRecord): string {
  if (apply.status === "review_ready" && !reviewHasChanges(apply)) {
    return "No changes to apply";
  }

  switch (apply.status) {
    case "integrating":
      return "Preparing";
    case "review_ready":
      return "Ready for review";
    case "conflicted":
      return "Conflicts need review";
    case "resolving":
      return "Resolving conflicts";
    case "applied":
      return "Applied to main";
    case "failed":
      return "Apply failed";
  }
}

function reviewHasChanges(apply: SessionApplyRecord): boolean {
  return (
    (apply.review.diff?.files.length ?? 0) > 0 ||
    (apply.review.diff?.patch.trim().length ?? 0) > 0
  );
}

function reviewLineTotals(apply: SessionApplyRecord): {
  readonly additions: number;
  readonly deletions: number;
} {
  return (apply.review.diff?.files ?? []).reduce(
    (totals, file) => ({
      additions: totals.additions + file.additions,
      deletions: totals.deletions + file.deletions,
    }),
    { additions: 0, deletions: 0 },
  );
}

function statusTone(apply: SessionApplyRecord): DotTone {
  switch (apply.status) {
    case "review_ready":
    case "applied":
      return "success";
    case "conflicted":
      return "warning";
    case "failed":
      return "danger";
    case "integrating":
    case "resolving":
      return "live";
  }
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : undefined;
}
