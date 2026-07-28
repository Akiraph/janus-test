type RecordValue = Record<string, unknown>;

export interface SessionDiffLine {
  kind: string;
  oldNumber: number | null;
  newNumber: number | null;
  text: string;
}

export interface SessionDiffHunk {
  lines: SessionDiffLine[];
}

export interface SessionDiffFile {
  path: string;
  kind: string;
  binary: boolean;
  hunks: SessionDiffHunk[];
}

export function decodeSessionDiff(value: unknown): SessionDiffFile[] {
  const root = record(value);
  const summary = record(root.summary);
  const paths = Array.isArray(summary.paths)
    ? summary.paths
    : Array.isArray(summary.files)
      ? summary.files
      : [];

  return paths.map((value) => {
    const file = record(value);
    return {
      path: text(file.path ?? file.rel_path, "?"),
      kind: text(file.kind ?? file.change),
      binary: file.binary === true,
      hunks: decodeHunks(file.hunks),
    };
  });
}

function decodeHunks(value: unknown): SessionDiffHunk[] {
  if (!Array.isArray(value)) return [];
  return value.map((value) => {
    const hunk = record(value);
    return {
      lines: Array.isArray(hunk.lines)
        ? hunk.lines.map((value) => {
            const line = record(value);
            return {
              kind: text(line.kind),
              oldNumber: number(line.old_no),
              newNumber: number(line.new_no),
              text: text(line.text),
            };
          })
        : [],
    };
  });
}

function record(value: unknown): RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as RecordValue)
    : {};
}

function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function number(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
