import { EditorView, keymap } from "@codemirror/view";
import { vscodeLight } from "@uiw/codemirror-theme-vscode";
import CodeMirror, { type Extension } from "@uiw/react-codemirror";
import { useEffect, useMemo, useRef, useState } from "react";
import { useWorkspaceFile, useWriteWorkspaceFile } from "../hooks";

/**
 * Light editor chrome — layered on top of the `vscode` light syntax theme.
 * Aligns the editor's structural surfaces (gutter, active line, selection,
 * focus ring, corner radius) with the 雾空蓝 design tokens so the editor reads
 * as part of the light UI rather than a dark island.
 */
const editorChrome = EditorView.theme({
  "&": {
    backgroundColor: "hsl(var(--card))",
    color: "hsl(var(--foreground))",
  },
  ".cm-gutters": {
    backgroundColor: "hsl(var(--card))",
    color: "hsl(var(--faint))",
    borderRight: "1px solid hsl(var(--border))",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "hsl(var(--muted))",
    color: "hsl(var(--muted-foreground))",
  },
  ".cm-activeLine": { backgroundColor: "hsl(var(--muted) / 0.5)" },
  "&.cm-focused": { outline: "2px solid hsl(var(--border-accent-soft))" },
  ".cm-selectionBackground, ::selection": {
    backgroundColor: "hsl(var(--border-accent-soft))",
  },
  ".cm-content": { fontFamily: "var(--font-mono)" },
  ".cm-foldGutter .cm-gutterElement": {
    alignItems: "center",
    display: "flex",
    justifyContent: "center",
  },
  ".cm-foldGutter .cm-foldGutter-open, .cm-foldGutter .cm-foldGutter-folded": {
    color: "transparent",
    height: "16px",
    position: "relative",
    width: "16px",
  },
  ".cm-foldGutter .cm-foldGutter-open::after, .cm-foldGutter .cm-foldGutter-folded::after":
    {
      borderBottom: "1.5px solid hsl(var(--faint))",
      borderRight: "1.5px solid hsl(var(--faint))",
      content: '""',
      height: "6px",
      left: "5px",
      position: "absolute",
      top: "4px",
      transform: "rotate(-45deg)",
      transition: "transform 150ms ease, border-color 150ms ease",
      width: "6px",
    },
  ".cm-foldGutter .cm-foldGutter-open::after": {
    transform: "rotate(45deg)",
  },
  ".cm-foldGutter .cm-foldGutter-open:hover::after, .cm-foldGutter .cm-foldGutter-folded:hover::after":
    {
      borderColor: "hsl(var(--foreground))",
    },
  "&.cm-editor.cm-focused": {
    borderRadius: "var(--radius-md)",
  },
});

interface FileViewerProps {
  readonly projectId: string;
  readonly path: string;
  /** Notified whenever the editor's dirty state changes (lifted to the tab). */
  readonly onDirtyChange?: ((dirty: boolean) => void) | undefined;
  /** Registers the current save action so parent close-confirm dialogs can save. */
  readonly onSaveActionChange?:
    | ((action: (() => Promise<boolean>) | undefined) => void)
    | undefined;
}

/**
 * FileViewer — VSCode-style editor for a single workspace file, rendered as the
 * content of a file tab. Syntax highlighting via CodeMirror 6, in-place editing,
 * and Save (button or Ctrl/Cmd+S) that writes back to the server working tree.
 * Truncated files (over the read cap) open read-only so a save can't drop the
 * unseen tail. Dirty state is reported upward so the tab can render a dot.
 */
export function FileViewer({
  projectId,
  path,
  onDirtyChange,
  onSaveActionChange,
}: FileViewerProps) {
  const { data, isLoading, error } = useWorkspaceFile(projectId, path);
  const save = useWriteWorkspaceFile(projectId);

  const [value, setValue] = useState("");
  const [baseline, setBaseline] = useState("");
  const [language, setLanguage] = useState<Extension | null>(null);
  // Path whose content has been seeded into the editor — guards against
  // clobbering in-progress edits on a background refetch, and re-seeds when the
  // tab switches to a different file.
  const loadedPathRef = useRef<string | null>(null);

  const truncated = data?.file.truncated ?? false;
  const dirty = value !== baseline;

  // Lift dirty state to the parent (for the tab dot + close-prompt) whenever it
  // changes. Keep it stable so callers don't loop.
  const dirtyRef = useRef(dirty);
  if (dirtyRef.current !== dirty) {
    dirtyRef.current = dirty;
  }
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  // Seed editor content the first time each file loads (and on file switch,
  // since the ref still holds the previous path).
  useEffect(() => {
    if (data && loadedPathRef.current !== path) {
      loadedPathRef.current = path;
      setValue(data.file.content);
      setBaseline(data.file.content);
    }
  }, [data, path]);

  useEffect(() => {
    let cancelled = false;
    setLanguage(null);
    void Promise.all([
      import("@codemirror/language"),
      import("@codemirror/language-data"),
    ])
      .then(([languageModule, languageDataModule]) =>
        languageModule.LanguageDescription.matchFilename(
          languageDataModule.languages,
          basename(path),
        )?.load(),
      )
      .then((support) => {
        if (!cancelled && support !== undefined) {
          setLanguage(support);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const saveFile = async (): Promise<boolean> => {
    if (!dirty) {
      return true;
    }

    if (truncated || save.isPending) {
      return false;
    }

    const next = value;
    try {
      await save.mutateAsync({ path, content: next });
      setBaseline(next);
      return true;
    } catch {
      return false;
    }
  };

  // Keep the keymap's save closure pointing at the latest state without
  // reconfiguring the editor on every keystroke.
  const saveRef = useRef(saveFile);
  saveRef.current = saveFile;

  useEffect(() => {
    if (onSaveActionChange === undefined) {
      return undefined;
    }

    const saveCurrent = () => saveRef.current();
    onSaveActionChange(saveCurrent);
    return () => onSaveActionChange(undefined);
  }, [onSaveActionChange]);

  const extensions = useMemo<Extension[]>(() => {
    const exts: Extension[] = [
      editorChrome,
      keymap.of([
        {
          key: "Mod-s",
          run: () => {
            void saveRef.current();
            return true;
          },
        },
      ]),
    ];
    if (language) {
      exts.push(language);
    }
    return exts;
  }, [language]);

  return (
    <div className="flex h-full flex-col bg-card">
      {save.error && (
        <div className="shrink-0 border-b border-border bg-destructive/10 px-4 py-1.5 text-xs text-destructive">
          {save.error instanceof Error ? save.error.message : "Failed to save"}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-hidden">
        {isLoading && (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            Loading file…
          </div>
        )}
        {error && (
          <div className="flex h-full items-center justify-center p-4 text-center text-sm text-destructive">
            {error instanceof Error ? error.message : "Failed to load file"}
          </div>
        )}
        {data && (
          <CodeMirror
            value={value}
            height="100%"
            theme={vscodeLight}
            extensions={extensions}
            editable={!truncated}
            readOnly={truncated}
            onChange={setValue}
            basicSetup={{ lineNumbers: true, foldGutter: true }}
            className="h-full text-[13px]"
          />
        )}
      </div>
    </div>
  );
}

function basename(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}
