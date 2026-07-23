import { cn } from "../../lib/cn";

export interface CodeBlockProps {
  readonly children: React.ReactNode;
  readonly className?: string;
  readonly tone?: "default" | "danger";
}

/** A calm monospace surface for raw CLI output, diffs, and command logs. */
export function CodeBlock({
  children,
  className,
  tone = "default",
}: CodeBlockProps) {
  return (
    <pre
      className={cn(
        "max-h-64 overflow-auto rounded-md border px-3 py-2 font-mono text-2xs leading-relaxed",
        tone === "danger"
          ? "border-destructive/20 bg-destructive-soft/60 text-foreground"
          : "border-border bg-muted/60 text-muted-foreground",
        className,
      )}
    >
      {children}
    </pre>
  );
}
