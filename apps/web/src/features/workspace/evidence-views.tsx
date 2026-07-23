import type { SessionDiff, SessionDiffFile } from "@janus/shared";
import { CodeBlock } from "../../components/ui/code-block";
import { cn } from "../../lib/cn";
import { occurrenceKey } from "../../lib/format";

const STATUS_GLYPH: Record<
  SessionDiffFile["status"],
  { glyph: string; cls: string }
> = {
  added: { glyph: "A", cls: "text-success" },
  modified: { glyph: "M", cls: "text-warning" },
  deleted: { glyph: "D", cls: "text-destructive" },
  renamed: { glyph: "R", cls: "text-info" },
  untracked: { glyph: "U", cls: "text-faint" },
};

export function DiffView({
  diff,
  path,
}: {
  readonly diff: SessionDiff;
  readonly path?: string;
}) {
  const files =
    path === undefined
      ? diff.files
      : diff.files.filter((file) => file.path === path);
  const patch =
    path === undefined ? diff.patch : patchForPath(diff.patch, path);

  return (
    <div className="space-y-2">
      <ul className="divide-y divide-border overflow-hidden rounded-md border border-border bg-card">
        {files.map((file) => {
          const meta = STATUS_GLYPH[file.status];
          return (
            <li
              key={file.path}
              className="flex items-center gap-2 px-2.5 py-1.5 text-xs"
            >
              <span
                className={cn(
                  "grid h-4 w-4 shrink-0 place-items-center rounded-xs font-mono text-[10px] font-bold",
                  meta.cls,
                )}
              >
                {meta.glyph}
              </span>
              <span className="truncate font-mono text-muted-foreground">
                {file.path}
              </span>
              <span className="ml-auto shrink-0 font-mono text-[11px]">
                <span className="text-success">+{file.additions}</span>{" "}
                <span className="text-destructive">-{file.deletions}</span>
              </span>
            </li>
          );
        })}
      </ul>
      <CodeBlock>{colorizePatch(compactPatch(patch))}</CodeBlock>
    </div>
  );
}

function colorizePatch(patch: string): React.ReactNode {
  const lineKey = occurrenceKey();
  return patch.split("\n").map((line) => {
    let cls = "text-muted-foreground";
    if (line.startsWith("+") && !line.startsWith("+++")) {
      cls = "bg-success/10 text-success";
    } else if (line.startsWith("-") && !line.startsWith("---")) {
      cls = "bg-destructive/10 text-destructive";
    } else if (line.startsWith("@@")) {
      cls = "text-info";
    }
    return (
      <div key={lineKey(line)} className={cn("-mx-1 px-1", cls)}>
        {line || " "}
      </div>
    );
  });
}

function patchForPath(patch: string, path: string): string {
  const sections = patch
    .split(/(?=^diff --git )/m)
    .filter((section) => section.trim().length > 0);

  return (
    sections.find((section) => patchSectionMatchesPath(section, path)) ?? patch
  );
}

function patchSectionMatchesPath(section: string, path: string): boolean {
  const quotedPath = path.replace(/\\/g, "/");
  return (
    section.includes(` b/${quotedPath}`) ||
    section.includes(` a/${quotedPath}`) ||
    section.includes(`+++ b/${quotedPath}`) ||
    section.includes(`--- a/${quotedPath}`)
  );
}

function compactPatch(patch: string): string {
  return patch
    .split("\n")
    .filter((line) => !line.startsWith(" "))
    .join("\n");
}

export function RawLog({ lines }: { lines: readonly string[] }) {
  const lineKey = occurrenceKey();

  return (
    <CodeBlock>
      {lines.map((line) => (
        <div key={lineKey(line)}>{line || " "}</div>
      ))}
    </CodeBlock>
  );
}

export function FileList({ paths }: { paths: readonly string[] }) {
  return (
    <ul className="overflow-hidden rounded-md border border-border bg-card">
      {paths.map((path) => (
        <li
          key={path}
          className="border-b border-border px-2.5 py-1.5 font-mono text-xs text-muted-foreground last:border-b-0"
        >
          {path}
        </li>
      ))}
    </ul>
  );
}
