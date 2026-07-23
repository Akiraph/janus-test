import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { cn } from "../../../lib/cn";

type MarkdownOutputProps = {
  readonly text: string;
  readonly tone?: "default" | "muted" | "error";
};

export function MarkdownOutput({
  text,
  tone = "default",
}: MarkdownOutputProps) {
  const isError = tone === "error";

  return (
    <div
      className={cn(
        "text-sm leading-relaxed",
        tone === "muted" ? "text-muted-foreground" : "text-foreground",
        isError && "text-destructive",
      )}
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          code: ({ className, children, ...props }) => {
            const match = /language-(\w+)/.exec(className || "");
            const language = match ? match[1] : "";
            const isInline = !className || !language;

            if (!isInline && language) {
              return (
                <pre className="my-2 overflow-x-auto rounded-md border border-border bg-card p-3 text-foreground">
                  <code className={`language-${language} text-xs`} {...props}>
                    {children}
                  </code>
                </pre>
              );
            }

            return (
              <code
                className="rounded-xs bg-muted px-1 py-0.5 text-xs text-foreground"
                {...props}
              >
                {children}
              </code>
            );
          },
          a: ({ children, ...props }) => (
            <a
              className="text-info underline hover:no-underline"
              target="_blank"
              rel="noopener noreferrer"
              {...props}
            >
              {children}
            </a>
          ),
          p: ({ children, ...props }) => (
            <p className="mb-2 last:mb-0" {...props}>
              {children}
            </p>
          ),
          ul: ({ children, ...props }) => (
            <ul className="mb-2 list-disc pl-4 last:mb-0" {...props}>
              {children}
            </ul>
          ),
          ol: ({ children, ...props }) => (
            <ol className="mb-2 list-decimal pl-4 last:mb-0" {...props}>
              {children}
            </ol>
          ),
          table: ({ children, ...props }) => (
            <div className="my-3 overflow-x-auto rounded-md border border-border">
              <table
                className="w-full border-collapse text-left text-xs"
                {...props}
              >
                {children}
              </table>
            </div>
          ),
          thead: ({ children, ...props }) => (
            <thead className="bg-muted/80" {...props}>
              {children}
            </thead>
          ),
          th: ({ children, ...props }) => (
            <th
              className="border-b border-border px-3 py-2 font-semibold text-foreground"
              {...props}
            >
              {children}
            </th>
          ),
          td: ({ children, ...props }) => (
            <td
              className="border-t border-border px-3 py-2 align-top"
              {...props}
            >
              {children}
            </td>
          ),
          blockquote: ({ children, ...props }) => (
            <blockquote
              className="my-2 border-l-2 border-border pl-3 text-muted-foreground"
              {...props}
            >
              {children}
            </blockquote>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
