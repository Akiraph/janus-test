import { CodeBlock } from "../../../components/ui/code-block";
import { formatTerminalStreamText } from "../data/terminal-output";
import type { TerminalOutputViewModel } from "../types";

interface TerminalOutputSectionsProps {
  readonly output: TerminalOutputViewModel;
  readonly emptyState?: string;
}

export function TerminalOutputSections({
  output,
  emptyState = "No output.",
}: TerminalOutputSectionsProps) {
  const stdout = formatTerminalStreamText(output.stdout);
  const stderr = formatTerminalStreamText(output.stderr);
  const exitCode = output.exitCode;

  if (exitCode === undefined && stdout === undefined && stderr === undefined) {
    return <div className="text-muted-foreground">{emptyState}</div>;
  }

  return (
    <div className="space-y-2">
      {exitCode === undefined ? null : (
        <div className="font-mono text-2xs text-muted-foreground">
          exit_code:{" "}
          <span
            className={exitCode === 0 ? "text-success" : "text-destructive"}
          >
            {exitCode}
          </span>
        </div>
      )}
      {stdout === undefined ? null : (
        <TerminalStreamSection
          label="stdout"
          text={stdout}
          truncated={output.stdoutTruncated === true}
        />
      )}
      {stderr === undefined ? null : (
        <TerminalStreamSection
          label="stderr"
          text={stderr}
          truncated={output.stderrTruncated === true}
          tone="danger"
        />
      )}
    </div>
  );
}

function TerminalStreamSection({
  label,
  text,
  truncated,
  tone = "default",
}: {
  readonly label: "stdout" | "stderr";
  readonly text: string;
  readonly truncated: boolean;
  readonly tone?: "default" | "danger";
}) {
  return (
    <section className="space-y-1">
      <div className="flex items-center justify-between gap-2 px-0.5">
        <h4 className="font-mono text-2xs font-semibold text-muted-foreground">
          {label}
        </h4>
        {truncated ? (
          <span className="text-2xs text-warning">truncated</span>
        ) : null}
      </div>
      <CodeBlock tone={tone} className="max-h-48 whitespace-pre-wrap">
        {text}
      </CodeBlock>
    </section>
  );
}
