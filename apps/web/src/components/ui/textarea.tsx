import { forwardRef, useEffect, useRef } from "react";
import { cn } from "../../lib/cn";

export interface TextareaProps
  extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  readonly autoResize?: boolean;
}

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaProps>(
  ({ className, autoResize = false, ...props }, ref) => {
    const internalRef = useRef<HTMLTextAreaElement | null>(null);

    // Handle ref forwarding
    const setRefs = (el: HTMLTextAreaElement | null) => {
      internalRef.current = el;
      if (typeof ref === "function") {
        ref(el);
      } else if (ref) {
        ref.current = el;
      }
    };

    useEffect(() => {
      if (!autoResize || !internalRef.current) return;

      const textarea = internalRef.current;
      const adjustHeight = () => {
        textarea.style.height = "auto";
        textarea.style.height = `${textarea.scrollHeight}px`;
      };

      adjustHeight();
      textarea.addEventListener("input", adjustHeight);

      return () => {
        textarea.removeEventListener("input", adjustHeight);
      };
    }, [autoResize]);

    return (
      <textarea
        ref={setRefs}
        className={cn(
          "flex min-h-[80px] w-full rounded-sm border border-border bg-card px-3 py-2 text-sm transition-[border-color,box-shadow] duration-200 ease-out placeholder:text-muted-foreground hover:border-border-strong focus-visible:outline-none focus-visible:border-border-strong focus-visible:shadow-focus disabled:cursor-not-allowed disabled:opacity-50",
          autoResize && "resize-none overflow-hidden",
          className,
        )}
        {...props}
      />
    );
  },
);
Textarea.displayName = "Textarea";
