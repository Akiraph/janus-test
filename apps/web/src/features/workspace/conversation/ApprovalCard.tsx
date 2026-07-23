import type { RuntimeApprovalRequest } from "@janus/shared";
import { ShieldAlert } from "lucide-react";
import { Badge } from "../../../components/ui/badge";
import { Button } from "../../../components/ui/button";
import { cn } from "../../../lib/cn";
import { riskVisual } from "../../../lib/status";

export interface ApprovalCardProps {
  readonly approval: RuntimeApprovalRequest;
  readonly onApprove: () => void;
  readonly onDeny: () => void;
}

export function ApprovalCard({
  approval,
  onApprove,
  onDeny,
}: ApprovalCardProps) {
  const risk = riskVisual(approval.riskLevel);
  const high = approval.riskLevel === "high";
  return (
    <div
      className={cn(
        "animate-fade-in-up rounded-lg border bg-card p-3 shadow-raised",
        high ? "border-destructive/40" : "border-warning/40",
      )}
    >
      <div className="flex items-start gap-2.5">
        <span
          className={cn(
            "grid h-7 w-7 shrink-0 place-items-center rounded-sm",
            high
              ? "bg-destructive-soft text-destructive"
              : "bg-warning-soft text-warning",
          )}
        >
          <ShieldAlert size={15} aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-sm font-semibold">{approval.title}</span>
            <Badge tone={risk.tone}>{approval.riskLevel} risk</Badge>
            <Badge tone="neutral">
              {approval.actionKind.replace(/_/g, " ")}
            </Badge>
            <span className="text-2xs text-faint">
              via {approval.source.replace(/_/g, " ")}
            </span>
          </div>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
            {approval.description}
          </p>
          <div className="mt-2.5 flex items-center gap-2">
            <Button variant="destructive" size="sm" onClick={onDeny}>
              Deny
            </Button>
            <Button variant="success" size="sm" onClick={onApprove}>
              Approve
            </Button>
            <button
              type="button"
              className="text-2xs text-faint underline-offset-2 hover:underline"
            >
              add note…
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
