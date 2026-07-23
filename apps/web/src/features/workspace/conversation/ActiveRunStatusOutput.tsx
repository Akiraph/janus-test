import type { ActivityEvent, SupervisorRunRecord } from "@janus/shared";
import { StatusDot } from "../../../components/ui/status-dot";
import { SupervisorOutput } from "./SupervisorOutput";

export function ActiveRunStatusOutput({
  run,
  retryEvent,
}: {
  readonly run: SupervisorRunRecord;
  readonly retryEvent?: ActivityEvent | undefined;
}) {
  switch (run.status) {
    case "queued":
      return <SupervisorOutput text="Queued." tone="muted" />;
    case "running": {
      if (retryEvent !== undefined) {
        return <RetryStatusOutput message={retryEvent.message} />;
      }

      const text = activeRunStatusText(run);

      return <SupervisorOutput text={text} tone="muted" />;
    }
    case "completed":
    case "canceled":
    case "failed":
      return null;
  }
}

export function latestSupervisorModelRetryEvent(
  events: readonly ActivityEvent[],
  run: SupervisorRunRecord | undefined,
): ActivityEvent | undefined {
  if (run === undefined || run.status !== "running") {
    return undefined;
  }

  const latestModelConnectionEvent = events
    .filter(
      (event) =>
        event.type === "supervisor_model_retry" ||
        event.type === "supervisor_model_recovered",
    )
    .filter((event) => event.sessionId === run.sessionId)
    .sort((left, right) => left.sequence - right.sequence)
    .at(-1);

  return latestModelConnectionEvent?.type === "supervisor_model_retry"
    ? latestModelConnectionEvent
    : undefined;
}

function RetryStatusOutput({ message }: { readonly message: string }) {
  return (
    <div className="flex gap-2 rounded-sm border border-warning/30 bg-warning/10 px-3 py-2 text-sm text-foreground">
      <StatusDot tone="warning" pulse className="mt-1.5 shrink-0" />
      <div className="min-w-0">
        <p className="font-medium">Model connection interrupted</p>
        <p className="mt-0.5 break-words text-muted-foreground">{message}</p>
      </div>
    </div>
  );
}

function activeRunStatusText(run: SupervisorRunRecord): string {
  if (run.askRequests?.some((ask) => ask.status === "pending") === true) {
    return "Waiting for your answer...";
  }

  if (run.cliJobs?.some((job) => job.status === "running") === true) {
    return "Waiting for CLI job to finish...";
  }

  if (
    run.groupDiscussions?.some(
      (discussion) => discussion.status === "running",
    ) === true
  ) {
    return "Waiting for group discussion to finish...";
  }

  return "Working...";
}
