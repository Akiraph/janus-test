import { cva, type VariantProps } from "class-variance-authority";
import { forwardRef } from "react";
import { cn } from "../../lib/cn";

const iconTileVariants = cva(
  "inline-flex items-center justify-center rounded-xs shrink-0",
  {
    variants: {
      variant: {
        blue: "bg-info-soft text-foreground",
        green: "bg-success-soft text-success",
        amber: "bg-warning-soft text-warning",
        neutral: "bg-muted text-muted-foreground",
      },
      size: {
        sm: "h-8 w-8",
        md: "h-10 w-10",
        lg: "h-12 w-12",
      },
    },
    defaultVariants: {
      variant: "blue",
      size: "md",
    },
  },
);

export interface IconTileProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof iconTileVariants> {}

export const IconTile = forwardRef<HTMLDivElement, IconTileProps>(
  ({ className, variant, size, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cn(iconTileVariants({ variant, size }), className)}
        {...props}
      />
    );
  },
);
IconTile.displayName = "IconTile";

export { iconTileVariants };
