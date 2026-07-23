import { cn } from "../../lib/cn";

type DotTone =
  | "live"
  | "success"
  | "warning"
  | "danger"
  | "muted"
  | "primary"
  | "secondary";

const toneClass: Record<DotTone, string> = {
  live: "bg-info",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-destructive",
  muted: "bg-faint",
  primary: "bg-info",
  secondary: "bg-faint",
};

export interface StatusDotProps {
  readonly tone: DotTone;
  /** Pulse to signal an in-progress / live state. */
  readonly pulse?: boolean;
  readonly className?: string;
}

export function StatusDot({ tone, pulse = false, className }: StatusDotProps) {
  return (
    <span className={cn("relative inline-flex h-2 w-2", className)}>
      {pulse ? (
        <span
          className={cn(
            "absolute inline-flex h-full w-full rounded-full opacity-60 animate-live-pulse",
            toneClass[tone],
          )}
        />
      ) : null}
      <span
        className={cn(
          "relative inline-flex h-2 w-2 rounded-full",
          toneClass[tone],
        )}
      />
    </span>
  );
}

export type { DotTone };
