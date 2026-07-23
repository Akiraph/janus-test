import { cva, type VariantProps } from "class-variance-authority";
import { Loader2 } from "lucide-react";
import { forwardRef } from "react";
import { cn } from "../../lib/cn";

const statusChipVariants = cva(
  "inline-flex items-center gap-1.5 rounded-xs px-2.5 py-0.5 text-xs font-medium transition-colors",
  {
    variants: {
      status: {
        ready: "bg-success-soft text-success border border-success/20",
        cloning:
          "bg-warning-soft text-warning border border-warning/20 animate-pulse",
        failed:
          "bg-destructive-soft text-destructive border border-destructive/20",
        completed: "bg-success-soft text-success border border-success/20",
        delivering: "bg-info-soft text-info border border-info/20",
        running: "bg-info-soft text-info border border-info/20",
      },
    },
    defaultVariants: {
      status: "ready",
    },
  },
);

const statusDotVariants = cva("h-1.5 w-1.5 rounded-full shrink-0", {
  variants: {
    status: {
      ready: "bg-success",
      cloning: "bg-warning",
      failed: "bg-destructive",
      completed: "bg-success",
      delivering: "bg-info",
      running: "bg-info",
    },
  },
  defaultVariants: {
    status: "ready",
  },
});

export interface StatusChipProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof statusChipVariants> {
  readonly label: string;
}

export const StatusChip = forwardRef<HTMLDivElement, StatusChipProps>(
  ({ className, status, label, ...props }, ref) => {
    const showSpinner = status === "cloning" || status === "running";

    return (
      <div
        ref={ref}
        className={cn(statusChipVariants({ status }), className)}
        {...props}
      >
        {showSpinner ? (
          <Loader2 className="h-3 w-3 animate-spin" />
        ) : (
          <span className={statusDotVariants({ status })} />
        )}
        <span>{label}</span>
      </div>
    );
  },
);
StatusChip.displayName = "StatusChip";

export { statusChipVariants };
