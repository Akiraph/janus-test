import type { SupervisorRunRecord } from "@janus/shared";
import { Loader2, X, Zap } from "lucide-react";
import { StatusDot } from "../../../components/ui/status-dot";

export function QueuedMessagesBar({
  runs,
  deliveringRunId,
  onDeliver,
  deletingRunId,
  onDelete,
}: {
  readonly runs: readonly SupervisorRunRecord[];
  readonly deliveringRunId?: string | undefined;
  readonly onDeliver: (runId: string) => void;
  readonly deletingRunId?: string | undefined;
  readonly onDelete: (runId: string) => void;
}) {
  return (
    <div className="space-y-1">
      {runs.map((run) => {
        const deliveryRequested = run.deliveryRequestedAt !== undefined;
        const delivering = deliveringRunId === run.id;
        const deleting = deletingRunId === run.id;
        const interrupting =
          run.deliveryIntent === "interrupt" && deliveryRequested;

        return (
          <div
            key={run.id}
            className="flex min-w-0 items-center gap-2 rounded-sm border border-border bg-background px-2 py-1.5 text-xs text-muted-foreground"
          >
            <StatusDot
              tone={deliveryRequested ? "live" : "muted"}
              pulse={deliveryRequested}
            />
            <span className="shrink-0 font-medium text-foreground">
              {interrupting
                ? "Interrupting"
                : deliveryRequested
                  ? "Sending"
                  : "Queued"}
            </span>
            <span className="min-w-0 flex-1 truncate">{run.task}</span>
            <button
              type="button"
              onClick={() => onDeliver(run.id)}
              disabled={deliveryRequested || delivering}
              title="Interrupt current run"
              aria-label="Interrupt current run with queued message"
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              {delivering ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Zap className="h-3.5 w-3.5" />
              )}
            </button>
            <button
              type="button"
              onClick={() => onDelete(run.id)}
              disabled={deleting}
              title="Delete queued message"
              aria-label="Delete queued message"
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              {deleting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <X className="h-3.5 w-3.5" />
              )}
            </button>
          </div>
        );
      })}
    </div>
  );
}
