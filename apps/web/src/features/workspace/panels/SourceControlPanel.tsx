import type { GitCommit, GitFileChange } from "@janus/shared";
import {
  CheckCircle2,
  CircleDot,
  Clock3,
  FileDiff,
  GitBranch,
  GitCommitHorizontal,
  Minus,
  Plus,
  RefreshCw,
} from "lucide-react";
import { useMemo, useState } from "react";
import { Badge } from "../../../components/ui/badge";
import { Button } from "../../../components/ui/button";
import { Textarea } from "../../../components/ui/textarea";
import { Tooltip } from "../../../components/ui/tooltip";
import { cn } from "../../../lib/cn";
import { ago } from "../../../lib/format";
import {
  useCommitGitChangesMutation,
  useGitHistoryQuery,
  useGitStatusQuery,
  useStageAllGitFilesMutation,
  useStageGitFileMutation,
  useUnstageGitFileMutation,
} from "../data/queries";
import { buildCommitGraph, type CommitGraphRow } from "./commit-graph";

interface SourceControlPanelProps {
  readonly projectId: string;
  readonly onFileSelect?: ((path: string) => void) | undefined;
}

export function SourceControlPanel({
  projectId,
  onFileSelect,
}: SourceControlPanelProps) {
  const [message, setMessage] = useState("");
  const [actionError, setActionError] = useState<string | undefined>();
  const statusQuery = useGitStatusQuery(projectId);
  const historyQuery = useGitHistoryQuery(projectId);
  const stageFile = useStageGitFileMutation(projectId);
  const unstageFile = useUnstageGitFileMutation(projectId);
  const stageAll = useStageAllGitFilesMutation(projectId);
  const commit = useCommitGitChangesMutation(projectId);
  const status = statusQuery.data;
  const stagedCount = status?.stagedChanges.length ?? 0;
  const changeCount = status?.changes.length ?? 0;
  const busy =
    stageFile.isPending ||
    unstageFile.isPending ||
    stageAll.isPending ||
    commit.isPending;
  const canCommit = message.trim().length > 0 && stagedCount > 0 && !busy;

  const runAction = async (action: () => Promise<unknown>) => {
    setActionError(undefined);
    try {
      await action();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : "Git failed.");
    }
  };

  const handleCommit = async () => {
    await runAction(async () => {
      await commit.mutateAsync(message);
      setMessage("");
    });
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-border px-3">
        <GitBranch className="h-4 w-4 shrink-0 text-muted-foreground" />
        <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">
          Source Control
        </h2>
        <Tooltip content="Refresh source control" side="bottom">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Refresh source control"
            onClick={() => {
              setActionError(undefined);
              void statusQuery.refetch();
              void historyQuery.refetch();
            }}
            disabled={statusQuery.isFetching}
          >
            <RefreshCw
              size={14}
              aria-hidden
              className={cn(statusQuery.isFetching && "animate-spin")}
            />
          </Button>
        </Tooltip>
      </div>

      <div className="min-h-0 flex-1 overflow-auto px-2.5 pb-3 pt-2">
        {statusQuery.isLoading ? (
          <p className="px-1.5 py-2 text-xs text-muted-foreground">
            Loading source control...
          </p>
        ) : statusQuery.isError ? (
          <ErrorMessage
            message={
              statusQuery.error instanceof Error
                ? statusQuery.error.message
                : "Could not load source control."
            }
          />
        ) : status ? (
          <div className="space-y-3">
            <div className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground">
              <GitBranch size={13} aria-hidden />
              <span className="min-w-0 flex-1 truncate font-mono">
                {status.branch}
              </span>
              {status.clean ? (
                <Badge tone="success">clean</Badge>
              ) : (
                <Badge tone="warning">{stagedCount + changeCount}</Badge>
              )}
            </div>

            <div className="space-y-2">
              <Textarea
                value={message}
                onChange={(event) => setMessage(event.target.value)}
                placeholder="Message"
                rows={3}
                className="min-h-[68px] resize-none px-2.5 py-2 text-xs"
              />
              <Button
                type="button"
                variant="primary"
                size="sm"
                className="w-full"
                disabled={!canCommit}
                onClick={() => void handleCommit()}
              >
                <GitCommitHorizontal size={13} aria-hidden />
                Commit
              </Button>
            </div>

            {actionError ? <ErrorMessage message={actionError} /> : null}

            {status.clean ? (
              <div className="flex items-start gap-2 rounded-md border border-border bg-card px-2.5 py-2">
                <CheckCircle2
                  size={15}
                  className="mt-0.5 shrink-0 text-success"
                  aria-hidden
                />
                <p className="text-xs text-muted-foreground">
                  No local changes.
                </p>
              </div>
            ) : null}

            <ChangeGroup
              title="Staged changes"
              count={stagedCount}
              changes={status.stagedChanges}
              actionLabel="Unstage"
              actionIcon="minus"
              busy={busy}
              onOpenPath={onFileSelect}
              headerAction={
                stagedCount > 0 ? (
                  <Tooltip content="Unstage all staged changes" side="bottom">
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-sm"
                      aria-label="Unstage all staged changes"
                      disabled={busy}
                      onClick={() =>
                        void runAction(() =>
                          Promise.all(
                            status.stagedChanges.map((change) =>
                              unstageFile.mutateAsync(change.path),
                            ),
                          ),
                        )
                      }
                    >
                      <Minus size={13} aria-hidden />
                    </Button>
                  </Tooltip>
                ) : null
              }
              onAction={(path) =>
                void runAction(() => unstageFile.mutateAsync(path))
              }
            />

            <ChangeGroup
              title="Changes"
              count={changeCount}
              changes={status.changes}
              actionLabel="Stage"
              actionIcon="plus"
              busy={busy}
              onOpenPath={onFileSelect}
              headerAction={
                changeCount > 0 ? (
                  <Tooltip content="Stage all changes" side="bottom">
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-sm"
                      aria-label="Stage all changes"
                      disabled={busy}
                      onClick={() =>
                        void runAction(() => stageAll.mutateAsync())
                      }
                    >
                      <Plus size={13} aria-hidden />
                    </Button>
                  </Tooltip>
                ) : null
              }
              onAction={(path) =>
                void runAction(() => stageFile.mutateAsync(path))
              }
            />

            <CommitTree
              commits={historyQuery.data?.commits ?? []}
              branches={historyQuery.data?.branches ?? []}
              isLoading={
                historyQuery.data === undefined && historyQuery.isFetching
              }
              isFetching={historyQuery.isFetching}
              error={historyQuery.error}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}

interface ChangeGroupProps {
  readonly title: string;
  readonly count: number;
  readonly changes: readonly GitFileChange[];
  readonly actionLabel: string;
  readonly actionIcon: "plus" | "minus";
  readonly busy: boolean;
  readonly headerAction?: React.ReactNode;
  readonly onOpenPath?: ((path: string) => void) | undefined;
  readonly onAction: (path: string) => void;
}

function ChangeGroup({
  title,
  count,
  changes,
  actionLabel,
  actionIcon,
  busy,
  headerAction,
  onOpenPath,
  onAction,
}: ChangeGroupProps) {
  return (
    <section>
      <div className="mb-1 flex h-7 items-center gap-1 px-1">
        <span className="text-2xs font-semibold text-faint">{title}</span>
        <Badge tone="neutral" className="ml-1">
          {count}
        </Badge>
        <div className="ml-auto">{headerAction}</div>
      </div>
      {changes.length === 0 ? (
        <p className="px-1.5 py-1 text-2xs text-faint">None</p>
      ) : (
        <div className="space-y-0.5">
          {changes.map((change) => (
            <ChangeRow
              key={`${change.status}:${change.path}`}
              change={change}
              actionLabel={actionLabel}
              actionIcon={actionIcon}
              busy={busy}
              onOpenPath={onOpenPath}
              onAction={() => onAction(change.path)}
            />
          ))}
        </div>
      )}
    </section>
  );
}

interface ChangeRowProps {
  readonly change: GitFileChange;
  readonly actionLabel: string;
  readonly actionIcon: "plus" | "minus";
  readonly busy: boolean;
  readonly onOpenPath?: ((path: string) => void) | undefined;
  readonly onAction: () => void;
}

function ChangeRow({
  change,
  actionLabel,
  actionIcon,
  busy,
  onOpenPath,
  onAction,
}: ChangeRowProps) {
  const Icon = actionIcon === "plus" ? Plus : Minus;
  return (
    <div className="group flex min-h-8 items-center gap-1 rounded-md px-1.5 py-1 transition-colors duration-150 hover:bg-card">
      <FileDiff size={14} className="shrink-0 text-faint" aria-hidden />
      <button
        type="button"
        className="min-w-0 flex-1 text-left disabled:cursor-default"
        disabled={onOpenPath === undefined}
        onClick={() => onOpenPath?.(change.path)}
      >
        <div className="truncate text-xs text-foreground">
          {basename(change.path)}
        </div>
        <div className="truncate text-[10px] text-faint">
          {dirname(change.path)}
        </div>
      </button>
      <span
        className={cn(
          "w-4 shrink-0 text-center text-[10px] font-semibold",
          statusClass(change.status),
        )}
      >
        {statusLabel(change.status)}
      </span>
      <Tooltip content={actionLabel} side="bottom">
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={`${actionLabel} ${change.path}`}
          disabled={busy}
          onClick={onAction}
          className="opacity-70 group-hover:opacity-100"
        >
          <Icon size={13} aria-hidden />
        </Button>
      </Tooltip>
    </div>
  );
}

function CommitTree({
  commits,
  branches,
  isLoading,
  isFetching,
  error,
}: {
  readonly commits: readonly GitCommit[];
  readonly branches: readonly string[];
  readonly isLoading: boolean;
  readonly isFetching: boolean;
  readonly error: Error | null;
}) {
  const now = Date.now();
  const graph = useMemo(() => buildCommitGraph(commits), [commits]);

  return (
    <section>
      <div className="mb-1 flex h-7 items-center gap-1 px-1">
        <Clock3 size={13} aria-hidden className="text-faint" />
        <span className="text-2xs font-semibold text-faint">History</span>
        <Badge tone="neutral" className="ml-1">
          {commits.length}
        </Badge>
        {branches.length > 0 ? (
          <span className="ml-auto min-w-0 truncate font-mono text-[10px] text-faint">
            {branches.join(", ")}
          </span>
        ) : null}
        {isFetching && !isLoading ? (
          <RefreshCw
            size={12}
            aria-hidden
            className="animate-spin text-faint"
          />
        ) : null}
      </div>
      {isLoading ? (
        <p className="px-1.5 py-1 text-2xs text-faint">
          Loading commit history...
        </p>
      ) : error !== null ? (
        <ErrorMessage
          message={`Commit history unavailable: ${error.message}`}
        />
      ) : commits.length === 0 ? (
        <div className="rounded-md border border-border bg-card px-2.5 py-2">
          <p className="text-xs text-muted-foreground">
            No commits returned for this branch.
          </p>
          <p className="mt-0.5 text-2xs text-faint">
            The repository may be new, detached, or the history endpoint
            returned an empty branch log.
          </p>
        </div>
      ) : (
        <ol className="relative space-y-0.5">
          {graph.rows.map((row, index) => (
            <CommitTreeRow
              key={row.commit.sha}
              row={row}
              laneCount={graph.laneCount}
              now={now}
              isFirst={index === 0}
              isLatest={index === 0}
            />
          ))}
        </ol>
      )}
    </section>
  );
}

function CommitTreeRow({
  row,
  laneCount,
  now,
  isFirst,
  isLatest,
}: {
  readonly row: CommitGraphRow;
  readonly laneCount: number;
  readonly now: number;
  readonly isFirst: boolean;
  readonly isLatest: boolean;
}) {
  const { commit } = row;
  const parents = commit.parentShas ?? [];
  const isMerge = parents.length > 1;

  return (
    <li className="grid grid-cols-[minmax(34px,auto)_minmax(0,1fr)] gap-1.5 rounded-md px-1 py-1 transition-colors duration-150 hover:bg-card">
      <CommitGraphCell row={row} laneCount={laneCount} isFirst={isFirst} />
      <div className="min-w-0">
        <div className="flex min-w-0 items-start gap-1.5">
          <div className="line-clamp-2 min-w-0 flex-1 text-xs leading-snug text-foreground">
            {commit.message}
          </div>
          {isLatest ? <Badge tone="primary">latest</Badge> : null}
        </div>
        <div className="mt-0.5 flex flex-wrap items-center gap-1.5 text-[10px] text-faint">
          <span className="font-mono">{commit.sha.slice(0, 7)}</span>
          {parents.length > 0 ? (
            <>
              <span>parents</span>
              <span className="truncate font-mono">{parents.join(", ")}</span>
            </>
          ) : null}
          {isMerge ? <Badge tone="primary">merge</Badge> : null}
          {row.opensBranch ? <Badge tone="neutral">branch</Badge> : null}
          {commit.byJanus ? <Badge tone="success">Janus</Badge> : null}
          <span>{commit.author}</span>
          <span>{ago(commit.at, now)} ago</span>
        </div>
      </div>
    </li>
  );
}

function CommitGraphCell({
  row,
  laneCount,
  isFirst,
}: {
  readonly row: CommitGraphRow;
  readonly laneCount: number;
  readonly isFirst: boolean;
}) {
  const laneWidth = 12;
  const rowHeight = 46;
  const nodeY = 14;
  const graphWidth = Math.max(34, laneCount * laneWidth + 10);
  const laneX = (lane: number) => lane * laneWidth + 8;
  const activeLaneCount = Math.max(
    laneCount,
    row.lanesBefore.length,
    row.lanesAfter.length,
  );
  const commitX = laneX(row.commitLane);
  const lineClass = "stroke-border";
  const nodeClass =
    row.parentLanes.length > 1
      ? "fill-background stroke-info text-info"
      : "fill-background stroke-border text-faint";
  const renderedLanes = Array.from({ length: activeLaneCount }, (_, lane) => ({
    id: `${row.commit.sha}:${row.lanesBefore[lane] ?? ""}:${
      row.lanesAfter[lane] ?? ""
    }:${laneX(lane)}`,
    lane,
    hasBefore: row.lanesBefore[lane] !== undefined,
    hasAfter: row.lanesAfter[lane] !== undefined,
  })).filter((lane) => lane.hasBefore || lane.hasAfter);

  return (
    <svg
      className="shrink-0 overflow-visible"
      width={graphWidth}
      height={rowHeight}
      viewBox={`0 0 ${graphWidth} ${rowHeight}`}
      role="img"
      aria-labelledby={`commit-graph-${row.commit.sha}`}
    >
      <title id={`commit-graph-${row.commit.sha}`}>
        Commit graph for {row.commit.sha}
      </title>
      {renderedLanes.map(({ id, lane, hasBefore, hasAfter }) => {
        return (
          <g key={id}>
            {hasBefore && !isFirst ? (
              <line
                x1={laneX(lane)}
                y1={0}
                x2={laneX(lane)}
                y2={nodeY}
                className={lineClass}
                strokeWidth="1.25"
              />
            ) : null}
            {hasAfter ? (
              <line
                x1={laneX(lane)}
                y1={nodeY}
                x2={laneX(lane)}
                y2={rowHeight}
                className={lineClass}
                strokeWidth="1.25"
              />
            ) : null}
          </g>
        );
      })}
      {row.parentLanes.map((parentLane) =>
        parentLane === row.commitLane ? null : (
          <path
            key={`edge:${parentLane}`}
            d={`M ${commitX} ${nodeY} C ${commitX} 26, ${laneX(
              parentLane,
            )} 26, ${laneX(parentLane)} ${rowHeight}`}
            className="stroke-info"
            fill="none"
            strokeWidth="1.25"
          />
        ),
      )}
      <CircleDot
        x={commitX - 5}
        y={nodeY - 5}
        width={10}
        height={10}
        className={nodeClass}
        strokeWidth="1.5"
      />
    </svg>
  );
}

function ErrorMessage({ message }: { readonly message: string }) {
  return (
    <div className="rounded-md border border-destructive/30 bg-destructive-soft px-2.5 py-2 text-xs text-destructive">
      {message}
    </div>
  );
}

function statusLabel(status: GitFileChange["status"]): string {
  switch (status) {
    case "added":
      return "A";
    case "deleted":
      return "D";
    case "renamed":
      return "R";
    case "untracked":
      return "U";
    case "modified":
      return "M";
  }
}

function statusClass(status: GitFileChange["status"]): string {
  switch (status) {
    case "added":
    case "untracked":
      return "text-success";
    case "deleted":
      return "text-destructive";
    case "renamed":
      return "text-info";
    case "modified":
      return "text-warning";
  }
}

function basename(path: string): string {
  return path.split("/").at(-1) ?? path;
}

function dirname(path: string): string {
  const parts = path.split("/");
  parts.pop();
  return parts.length === 0 ? "." : parts.join("/");
}
