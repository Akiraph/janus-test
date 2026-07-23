import type { SupervisorCliJobStatus } from "@janus/shared";
import { ListTree, Square } from "lucide-react";
import { useMemo, useState } from "react";
import { Badge } from "../../../components/ui/badge";
import { Button } from "../../../components/ui/button";
import { CliBadge } from "../../../components/ui/cli-badge";
import { StatusDot } from "../../../components/ui/status-dot";
import { cn } from "../../../lib/cn";
import type { CliJobView } from "../types";
import { RailEmptyState } from "./SessionRightRail";
import { TerminalOutputSections } from "./TerminalOutputSections";

export { parseCliOutputLine } from "../data/terminal-output";

interface CliJobsSectionProps {
  readonly jobs: readonly CliJobView[];
  readonly onCancelJob?: ((runId: string) => void) | undefined;
  readonly cancelling?: boolean;
  readonly selectedJobId?: string | undefined;
  readonly onSelectedJobChange?: ((jobId: string) => void) | undefined;
}

/**
 * CliJobsSection — inline CLI jobs list + detail, rendered inside the session
 * right rail. Jobs sort newest-first; selecting one shows its stdout/stderr below.
 */
export function CliJobsSection({
  jobs,
  onCancelJob,
  cancelling = false,
  selectedJobId,
  onSelectedJobChange,
}: CliJobsSectionProps) {
  const [internalSelectedJobId, setInternalSelectedJobId] = useState<
    string | undefined
  >();

  const orderedJobs = useMemo(
    () =>
      [...jobs].sort((left, right) =>
        right.startedAt === left.startedAt
          ? right.id.localeCompare(left.id)
          : right.startedAt.localeCompare(left.startedAt),
      ),
    [jobs],
  );
  const currentSelectedJobId = selectedJobId ?? internalSelectedJobId;
  const selectedJob =
    orderedJobs.find((job) => job.id === currentSelectedJobId) ??
    orderedJobs[0];
  const selectJob = onSelectedJobChange ?? setInternalSelectedJobId;

  if (orderedJobs.length === 0) {
    return <RailEmptyState>No CLI jobs yet</RailEmptyState>;
  }

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex min-w-0 gap-1 overflow-x-auto pb-1">
        {orderedJobs.map((job) => (
          <button
            key={job.id}
            type="button"
            onClick={() => selectJob(job.id)}
            className={cn(
              "flex min-w-32 max-w-44 shrink-0 items-center gap-2 rounded-sm border border-border bg-background px-2 py-1.5 text-left text-2xs transition-colors hover:border-border-accent/70 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border-accent",
              selectedJob?.id === job.id && "border-border-accent bg-muted",
            )}
          >
            <CliBadge cli={job.cli} short />
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium text-foreground">
                {job.description}
              </span>
              <span className="mt-0.5 flex items-center gap-1.5 text-2xs text-muted-foreground">
                <StatusDot
                  tone={statusDotTone(job.status)}
                  pulse={job.status === "running"}
                />
                {statusLabel(job.status)}
              </span>
            </span>
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1">
        {selectedJob === undefined ? (
          <RailEmptyState>
            <span className="flex items-center gap-1.5">
              <ListTree className="h-3.5 w-3.5" aria-hidden />
              Select a CLI job
            </span>
          </RailEmptyState>
        ) : (
          <JobDetail
            job={selectedJob}
            cancelling={cancelling}
            onCancelJob={
              onCancelJob === undefined
                ? undefined
                : () => onCancelJob(selectedJob.runId)
            }
          />
        )}
      </div>
    </div>
  );
}

function JobDetail({
  job,
  onCancelJob,
  cancelling,
}: {
  readonly job: CliJobView;
  readonly onCancelJob?: (() => void) | undefined;
  readonly cancelling: boolean;
}) {
  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex min-w-0 items-start gap-2">
        <CliBadge cli={job.cli} />
        <div className="min-w-0 flex-1">
          <div className="truncate text-xs font-semibold text-foreground">
            {job.description}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-1.5">
            <JobStatusBadge status={job.status} />
            {job.exitCode !== undefined ? (
              <Badge tone={job.exitCode === 0 ? "success" : "danger"}>
                exit {job.exitCode}
              </Badge>
            ) : null}
          </div>
        </div>
        {job.status === "running" && onCancelJob !== undefined ? (
          <Button
            type="button"
            variant="destructive"
            size="sm"
            onClick={onCancelJob}
            disabled={cancelling}
          >
            <Square className="h-3.5 w-3.5" />
            Stop
          </Button>
        ) : null}
      </div>

      {job.stdoutTruncated || job.stderrTruncated ? (
        <div className="rounded-sm border border-warning/30 bg-warning-soft px-2 py-1.5 text-2xs text-warning">
          Output exceeded the stored limit and was truncated.
        </div>
      ) : null}

      <TerminalOutputSections
        output={job}
        emptyState={
          job.status === "running"
            ? "Waiting for first output..."
            : "No output."
        }
      />
    </div>
  );
}

function JobStatusBadge({
  status,
}: {
  readonly status: SupervisorCliJobStatus;
}) {
  switch (status) {
    case "running":
      return <Badge tone="info">running</Badge>;
    case "completed":
      return <Badge tone="success">completed</Badge>;
    case "failed":
      return <Badge tone="danger">failed</Badge>;
    case "canceled":
      return <Badge tone="secondary">canceled</Badge>;
  }
}

function statusLabel(status: SupervisorCliJobStatus): string {
  switch (status) {
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "failed":
      return "failed";
    case "canceled":
      return "canceled";
  }
}

function statusDotTone(
  status: SupervisorCliJobStatus,
): "live" | "success" | "danger" | "muted" {
  switch (status) {
    case "running":
      return "live";
    case "completed":
      return "success";
    case "failed":
      return "danger";
    case "canceled":
      return "muted";
  }
}
