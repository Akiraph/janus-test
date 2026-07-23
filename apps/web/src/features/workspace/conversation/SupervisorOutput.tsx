import { StatusDot } from "../../../components/ui/status-dot";
import { MarkdownOutput } from "./MarkdownOutput";

type SupervisorOutputProps = {
  readonly text: string;
  readonly tone?: "default" | "muted" | "error";
};

export function SupervisorOutput({
  text,
  tone = "default",
}: SupervisorOutputProps) {
  const isError = tone === "error";

  return (
    <div className="flex gap-2 px-1.5 py-1">
      <StatusDot
        tone={isError ? "danger" : "muted"}
        className="mt-1.5 shrink-0"
      />
      <div className="min-w-0 flex-1">
        <MarkdownOutput text={text} tone={tone} />
      </div>
    </div>
  );
}
