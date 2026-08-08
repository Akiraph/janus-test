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
  additions: number;
  deletions: number;
  binary: boolean;
  hunks: SessionDiffHunk[];
}

export interface SessionDiffConflict {
  direction: "sync" | "apply";
  paths: Array<{
    path: string;
    kind: string;
    baseHash: string | null;
    mainHash: string | null;
    sessionHash: string | null;
  }>;
}

export interface SessionDiffModel {
  files: SessionDiffFile[];
  syncEnabled: boolean;
  applyEnabled: boolean;
  pendingConflict: SessionDiffConflict | null;
}

export function decodeSessionDiff(value: unknown): SessionDiffModel {
  const root = record(value);
  const summary = Object.hasOwn(root, "summary") ? record(root.summary) : root;
  const paths = Array.isArray(summary.paths)
    ? summary.paths
    : Array.isArray(summary.files)
      ? summary.files
      : [];

  return {
    files: paths.map((value) => {
      const file = record(value);
      return {
        path: text(file.path ?? file.rel_path, "?"),
        kind: text(file.kind ?? file.change),
        additions: integer(file.additions),
        deletions: integer(file.deletions),
        binary: file.binary === true,
        hunks: decodeHunks(file.hunks),
      };
    }),
    syncEnabled: summary.sync_enabled === true,
    applyEnabled: summary.apply_enabled === true,
    pendingConflict: decodeConflict(summary.pending_conflict),
  };
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

function integer(value: unknown): number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : 0;
}

function decodeConflict(value: unknown): SessionDiffConflict | null {
  const conflict = record(value);
  const direction = conflict.direction;
  if ((direction !== "sync" && direction !== "apply") || !Array.isArray(conflict.paths)) {
    return null;
  }
  return {
    direction,
    paths: conflict.paths.map((value) => {
      const path = record(value);
      return {
        path: text(path.path, "?"),
        kind: text(path.kind, "modified"),
        baseHash: nullableText(path.base_hash),
        mainHash: nullableText(path.main_hash),
        sessionHash: nullableText(path.session_hash),
      };
    }),
  };
}

function nullableText(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}
