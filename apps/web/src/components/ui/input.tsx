import { cva, type VariantProps } from "class-variance-authority";
import { forwardRef, type ReactNode } from "react";
import { cn } from "../../lib/cn";

const inputVariants = cva(
  "flex w-full rounded-sm border bg-card px-3 py-2 text-sm transition-[border-color,box-shadow] duration-200 ease-out file:border-0 file:bg-transparent file:text-sm file:font-medium placeholder:text-muted-foreground focus-visible:outline-none focus-visible:shadow-focus disabled:cursor-not-allowed disabled:opacity-50",
  {
    variants: {
      variant: {
        default:
          "border-border hover:border-border-strong focus-visible:border-border-strong",
        error: "border-destructive focus-visible:shadow-focus-error",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

export interface InputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "prefix">,
    VariantProps<typeof inputVariants> {
  readonly error?: string;
  readonly prefix?: ReactNode;
  readonly suffix?: ReactNode;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(
  ({ className, variant, error, prefix, suffix, ...props }, ref) => {
    const hasAffixes = Boolean(prefix || suffix);

    if (!hasAffixes && !error) {
      return (
        <input
          ref={ref}
          className={cn(
            inputVariants({ variant: error ? "error" : variant }),
            className,
          )}
          aria-invalid={error ? "true" : undefined}
          aria-describedby={error ? `${props.id}-error` : undefined}
          {...props}
        />
      );
    }

    return (
      <div className="space-y-1">
        <div className="relative flex items-center">
          {prefix && (
            <div className="pointer-events-none absolute left-3 flex items-center text-muted-foreground">
              {prefix}
            </div>
          )}
          <input
            ref={ref}
            className={cn(
              inputVariants({ variant: error ? "error" : variant }),
              prefix && "pl-9",
              suffix && "pr-9",
              className,
            )}
            aria-invalid={error ? "true" : undefined}
            aria-describedby={error ? `${props.id}-error` : undefined}
            {...props}
          />
          {suffix && (
            <div className="pointer-events-none absolute right-3 flex items-center text-muted-foreground">
              {suffix}
            </div>
          )}
        </div>
        {error && (
          <p
            id={`${props.id}-error`}
            className="text-xs text-destructive"
            role="alert"
          >
            {error}
          </p>
        )}
      </div>
    );
  },
);
Input.displayName = "Input";

export { inputVariants };
