import type {
  ApprovalRiskLevel,
  SessionRuntimeHealthStatus,
  SupervisorRunStatus,
} from "@janus/shared";
import type { BadgeProps } from "../components/ui/badge";
import type { DotTone } from "../components/ui/status-dot";

type Tone = NonNullable<BadgeProps["tone"]>;

export interface StatusVisual {
  readonly label: string;
  readonly tone: Tone;
  readonly dot: DotTone;
  readonly pulse: boolean;
}

const LIVE: Pick<StatusVisual, "tone" | "dot" | "pulse"> = {
  tone: "primary",
  dot: "live",
  pulse: true,
};

export function runStatusVisual(status: SupervisorRunStatus): StatusVisual {
  switch (status) {
    case "queued":
      return { label: "Queued", ...LIVE };
    case "running":
      return { label: "Running", ...LIVE };
    case "completed":
      return {
        label: "Completed",
        tone: "success",
        dot: "success",
        pulse: false,
      };
    case "canceled":
      return {
        label: "Canceled",
        tone: "neutral",
        dot: "muted",
        pulse: false,
      };
    case "failed":
      return { label: "Failed", tone: "danger", dot: "danger", pulse: false };
  }
}

export function healthVisual(status: SessionRuntimeHealthStatus): StatusVisual {
  switch (status) {
    case "not_started":
      return {
        label: "Not started",
        tone: "neutral",
        dot: "muted",
        pulse: false,
      };
    case "healthy":
      return {
        label: "Healthy",
        tone: "success",
        dot: "success",
        pulse: false,
      };
    case "warning":
      return {
        label: "Warning",
        tone: "warning",
        dot: "warning",
        pulse: false,
      };
    case "failed":
      return { label: "Failed", tone: "danger", dot: "danger", pulse: false };
    case "completed":
      return { label: "Completed", tone: "info", dot: "primary", pulse: false };
  }
}

export function riskVisual(level: ApprovalRiskLevel): StatusVisual {
  switch (level) {
    case "low":
      return { label: "low", tone: "success", dot: "success", pulse: false };
    case "medium":
      return { label: "medium", tone: "warning", dot: "warning", pulse: false };
    case "high":
      return { label: "high", tone: "danger", dot: "danger", pulse: true };
  }
}
