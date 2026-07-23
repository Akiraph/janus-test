import { cva, type VariantProps } from "class-variance-authority";
import type { ReactNode } from "react";
import { cn } from "../../lib/cn";

const buttonGroupVariants = cva(
  "m-0 inline-flex min-w-0 rounded-md border border-border-strong bg-muted/50 p-0.5",
  {
    variants: {
      size: {
        sm: "gap-0.5",
        md: "gap-1",
      },
    },
    defaultVariants: { size: "md" },
  },
);

const buttonGroupItemVariants = cva(
  "relative inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-sm px-3 py-1.5 text-sm font-medium transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-border-accent/60 focus-visible:ring-offset-1 focus-visible:ring-offset-background disabled:pointer-events-none disabled:opacity-50",
  {
    variants: {
      selected: {
        true: "bg-border-accent text-foreground shadow-sm",
        false:
          "text-muted-foreground hover:bg-card hover:text-foreground data-[disabled]:hover:bg-transparent data-[disabled]:hover:text-muted-foreground",
      },
    },
    defaultVariants: { selected: false },
  },
);

export interface ButtonGroupProps
  extends React.HTMLAttributes<HTMLFieldSetElement>,
    VariantProps<typeof buttonGroupVariants> {
  readonly children: ReactNode;
}

export function ButtonGroup({
  className,
  size,
  children,
  ...props
}: ButtonGroupProps) {
  return (
    <fieldset
      className={cn(buttonGroupVariants({ size }), className)}
      {...props}
    >
      {children}
    </fieldset>
  );
}

export interface ButtonGroupItemProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  readonly selected?: boolean;
  readonly children: ReactNode;
}

export function ButtonGroupItem({
  className,
  selected = false,
  disabled = false,
  children,
  ...props
}: ButtonGroupItemProps) {
  return (
    <button
      type="button"
      className={cn(buttonGroupItemVariants({ selected }), className)}
      disabled={disabled}
      data-disabled={disabled ? "" : undefined}
      aria-pressed={selected}
      {...props}
    >
      {children}
    </button>
  );
}
