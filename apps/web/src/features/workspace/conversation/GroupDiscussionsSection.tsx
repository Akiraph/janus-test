import type { GroupDiscussionStatus } from "@janus/shared";
import { MessagesSquare } from "lucide-react";
import { useMemo, useState } from "react";
import { Badge } from "../../../components/ui/badge";
import { StatusDot } from "../../../components/ui/status-dot";
import { cn } from "../../../lib/cn";
import type { GroupDiscussionView } from "../types";
import { RailEmptyState } from "./SessionRightRail";

interface GroupDiscussionsSectionProps {
  readonly discussions: readonly GroupDiscussionView[];
}

export function GroupDiscussionsSection({
  discussions,
}: GroupDiscussionsSectionProps) {
  const [selectedId, setSelectedId] = useState<string | undefined>();
  const ordered = useMemo(
    () =>
      [...discussions].sort((left, right) =>
        right.startedAt === left.startedAt
          ? right.id.localeCompare(left.id)
          : right.startedAt.localeCompare(left.startedAt),
      ),
    [discussions],
  );
  const selected =
    ordered.find((discussion) => discussion.id === selectedId) ?? ordered[0];

  if (ordered.length === 0) {
    return <RailEmptyState>No group discussions yet</RailEmptyState>;
  }

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex min-h-0 flex-col gap-1 overflow-y-auto">
        {ordered.map((discussion) => (
          <button
            key={discussion.id}
            type="button"
            onClick={() => setSelectedId(discussion.id)}
            className={cn(
              "flex items-start gap-2 rounded-sm border border-border bg-card px-2 py-1.5 text-left text-2xs transition-colors hover:border-border-accent/70 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border-accent",
              selected?.id === discussion.id && "border-border-accent bg-muted",
            )}
          >
            <MessagesSquare
              className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground"
              aria-hidden
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium text-foreground">
                {discussion.topic}
              </span>
              <span className="mt-0.5 flex items-center gap-1.5 text-muted-foreground">
                <StatusDot
                  tone={statusDotTone(discussion.status)}
                  pulse={discussion.status === "running"}
                />
                {statusLabel(discussion.status)} ·{" "}
                {discussion.participants.length} models
              </span>
            </span>
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1">
        {selected === undefined ? (
          <RailEmptyState>Select a discussion</RailEmptyState>
        ) : (
          <DiscussionDetail discussion={selected} />
        )}
      </div>
    </div>
  );
}

function DiscussionDetail({
  discussion,
}: {
  readonly discussion: GroupDiscussionView;
}) {
  const completed = discussion.participants.filter(
    (participant) => participant.status === "completed",
  ).length;

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="min-w-0">
        <div className="truncate text-xs font-semibold text-foreground">
          {discussion.topic}
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-1.5">
          <DiscussionStatusBadge status={discussion.status} />
          <Badge tone="neutral">{depthLabel(discussion.depth)}</Badge>
          <Badge tone="neutral">
            {completed}/{discussion.participants.length} done
          </Badge>
        </div>
      </div>

      {discussion.summary ? (
        <div className="rounded-sm border border-border bg-card px-2 py-1.5 text-2xs leading-relaxed text-muted-foreground">
          {discussion.summary}
        </div>
      ) : null}

      <DiscussionList
        title="Recommendations"
        values={discussion.recommendations}
      />
      <DiscussionList title="Risks" values={discussion.risks} />
      <DiscussionList title="Disagreements" values={discussion.disagreements} />

      <div className="min-h-0 max-h-64 overflow-auto rounded-sm border border-border bg-muted/45 p-2">
        <div className="mb-1.5 text-2xs font-semibold text-faint">
          Participants
        </div>
        <div className="space-y-2">
          {discussion.participants.map((participant) => (
            <div
              key={participant.participantId}
              className="rounded-sm border border-border bg-card px-2 py-1.5"
            >
              <div className="flex min-w-0 items-center gap-1.5">
                <StatusDot
                  tone={participantDotTone(participant.status)}
                  pulse={participant.status === "running"}
                />
                <span className="min-w-0 flex-1 truncate text-2xs font-medium text-foreground">
                  {participant.displayName}
                </span>
                <span className="shrink-0 text-2xs text-faint">
                  {participant.status}
                </span>
              </div>
              {participant.stance ? (
                <p className="mt-1 text-2xs leading-relaxed text-muted-foreground">
                  {participant.stance}
                </p>
              ) : null}
              {participant.keyPoints.length > 0 ? (
                <ul className="mt-1 space-y-0.5 text-2xs leading-relaxed text-muted-foreground">
                  {participant.keyPoints.slice(0, 3).map((point) => (
                    <li key={point} className="flex gap-1.5">
                      <span className="mt-1 h-1 w-1 shrink-0 rounded-full bg-border-accent" />
                      <span className="min-w-0 break-words">{point}</span>
                    </li>
                  ))}
                </ul>
              ) : null}
              {participant.error ? (
                <p className="mt-1 text-2xs text-destructive">
                  {participant.error}
                </p>
              ) : null}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function DiscussionList({
  title,
  values,
}: {
  readonly title: string;
  readonly values: readonly string[];
}) {
  if (values.length === 0) {
    return null;
  }

  return (
    <div className="rounded-sm border border-border bg-card px-2 py-1.5">
      <div className="mb-1 text-2xs font-semibold text-faint">{title}</div>
      <ul className="space-y-0.5 text-2xs leading-relaxed text-muted-foreground">
        {values.slice(0, 4).map((value) => (
          <li key={value} className="flex gap-1.5">
            <span className="mt-1 h-1 w-1 shrink-0 rounded-full bg-border-accent" />
            <span className="min-w-0 break-words">{value}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function DiscussionStatusBadge({
  status,
}: {
  readonly status: GroupDiscussionStatus;
}) {
  switch (status) {
    case "running":
      return <Badge tone="info">running</Badge>;
    case "completed":
      return <Badge tone="success">completed</Badge>;
    case "partial":
      return <Badge tone="warning">partial</Badge>;
    case "failed":
      return <Badge tone="danger">failed</Badge>;
    case "cancelled":
      return <Badge tone="secondary">cancelled</Badge>;
  }
}

function statusLabel(status: GroupDiscussionStatus): string {
  switch (status) {
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "partial":
      return "partial";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
  }
}

function statusDotTone(
  status: GroupDiscussionStatus,
): "live" | "success" | "warning" | "danger" | "muted" {
  switch (status) {
    case "running":
      return "live";
    case "completed":
      return "success";
    case "partial":
      return "warning";
    case "failed":
      return "danger";
    case "cancelled":
      return "muted";
  }
}

function participantDotTone(
  status: GroupDiscussionView["participants"][number]["status"],
): "live" | "success" | "danger" | "muted" {
  switch (status) {
    case "running":
      return "live";
    case "completed":
      return "success";
    case "failed":
      return "danger";
    case "timeout":
      return "muted";
  }
}

function depthLabel(depth: GroupDiscussionView["depth"]): string {
  switch (depth) {
    case "first_pass":
      return "first pass";
    case "cross_review":
      return "cross review";
    case "deep_diverge":
      return "deep diverge";
  }
}
