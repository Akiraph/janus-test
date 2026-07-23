import { cva, type VariantProps } from "class-variance-authority";
import { forwardRef } from "react";
import { cn } from "../../lib/cn";

const cardVariants = cva(
  "rounded-md bg-card text-card-foreground transition-colors duration-200",
  {
    variants: {
      variant: {
        default: "border border-border shadow-card hover:border-border-strong",
        elevated: "border border-border shadow-raised",
        dashed:
          "border border-dashed border-border-strong hover:border-border hover:bg-muted/30",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

export interface CardProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof cardVariants> {}

export const Card = forwardRef<HTMLDivElement, CardProps>(
  ({ className, variant, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cn(cardVariants({ variant }), className)}
        {...props}
      />
    );
  },
);
Card.displayName = "Card";

export { cardVariants };
