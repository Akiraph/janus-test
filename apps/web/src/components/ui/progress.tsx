import { forwardRef } from "react";
import { cn } from "../../lib/cn";

export interface ProgressProps extends React.HTMLAttributes<HTMLDivElement> {
  readonly value: number;
  readonly label?: string;
  readonly showPercentage?: boolean;
}

export const Progress = forwardRef<HTMLDivElement, ProgressProps>(
  ({ className, value, label, showPercentage = false, ...props }, ref) => {
    const clampedValue = Math.min(Math.max(value, 0), 100);

    return (
      <div ref={ref} className={cn("space-y-1.5", className)} {...props}>
        {(label || showPercentage) && (
          <div className="flex items-center justify-between text-xs">
            {label && <span className="text-muted-foreground">{label}</span>}
            {showPercentage && (
              <span className="font-medium text-foreground">
                {clampedValue}%
              </span>
            )}
          </div>
        )}
        <div
          className="relative h-2 w-full overflow-hidden rounded-full bg-muted"
          role="progressbar"
          aria-valuenow={clampedValue}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label={label ?? "Progress"}
        >
          <div
            className="h-full bg-border-accent transition-all duration-300 ease-in-out"
            style={{ width: `${clampedValue}%` }}
          />
        </div>
      </div>
    );
  },
);
Progress.displayName = "Progress";
