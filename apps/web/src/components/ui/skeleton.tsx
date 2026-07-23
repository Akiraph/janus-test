import { cva, type VariantProps } from "class-variance-authority";
import { forwardRef } from "react";
import { cn } from "../../lib/cn";

const skeletonVariants = cva("animate-pulse bg-muted", {
  variants: {
    variant: {
      card: "rounded-md h-48 w-full",
      text: "rounded-xs h-4 w-full",
      circle: "rounded-full",
      rectangle: "rounded-xs",
    },
  },
  defaultVariants: {
    variant: "text",
  },
});

export interface SkeletonProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof skeletonVariants> {
  readonly width?: string | number;
  readonly height?: string | number;
}

export const Skeleton = forwardRef<HTMLDivElement, SkeletonProps>(
  ({ className, variant, width, height, style, ...props }, ref) => {
    const computedStyle = {
      ...style,
      ...(width && { width }),
      ...(height && { height }),
    };

    return (
      <div
        ref={ref}
        className={cn(skeletonVariants({ variant }), className)}
        style={computedStyle}
        {...props}
      />
    );
  },
);
Skeleton.displayName = "Skeleton";

export { skeletonVariants };
