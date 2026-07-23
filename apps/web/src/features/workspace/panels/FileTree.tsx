import type { WorkspaceTreeEntry } from "@janus/shared";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  File,
  Folder,
  FolderOpen,
  Pencil,
  RefreshCw,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { Input } from "../../../components/ui/input";
import { Tooltip } from "../../../components/ui/tooltip";
import { getWorkspaceFile } from "../../../lib/api-client";
import {
  useDeleteWorkspacePath,
  useRenameWorkspacePath,
  useWorkspaceTree,
  useWriteWorkspaceFile,
} from "../hooks";

interface FileTreeNodeProps {
  readonly entry: WorkspaceTreeEntry;
  readonly projectId: string;
  readonly depth: number;
  readonly busy: boolean;
  readonly renamingPath?: string | undefined;
  readonly renameDraft: string;
  readonly onFileSelect?: ((path: string) => void) | undefined;
  readonly onRenameDraftChange: (value: string) => void;
  readonly onSubmitRename: (entry: WorkspaceTreeEntry) => void;
  readonly onCancelRename: () => void;
  readonly onOpenContextMenu: (
    menu: Omit<FileTreeMenuState, "x" | "y">,
    point: { readonly x: number; readonly y: number },
  ) => void;
}

interface FileTreeMenuState {
  readonly entry: WorkspaceTreeEntry;
  readonly x: number;
  readonly y: number;
  readonly expanded: boolean;
  readonly onOpen: () => void;
  readonly onRefresh?: (() => void) | undefined;
}

interface UndoWorkspaceAction {
  readonly label: string;
  readonly run: () => Promise<unknown>;
}

const contextMenuWidth = 224;
const contextMenuHeight = 280;

function FileTreeNode({
  entry,
  projectId,
  depth,
  busy,
  renamingPath,
  renameDraft,
  onFileSelect,
  onRenameDraftChange,
  onSubmitRename,
  onCancelRename,
  onOpenContextMenu,
}: FileTreeNodeProps) {
  const [expanded, setExpanded] = useState(false);
  const isDirectory = entry.type === "dir";
  const isRenaming = renamingPath === entry.path;
  const inputRef = useRef<HTMLInputElement>(null);

  const {
    data: childData,
    isLoading,
    refetch,
  } = useWorkspaceTree(projectId, isDirectory && expanded ? entry.path : "", {
    enabled: isDirectory && expanded,
  });

  const hasChildren = childData?.entries && childData.entries.length > 0;

  useEffect(() => {
    if (!isRenaming) {
      return;
    }

    inputRef.current?.focus();
    inputRef.current?.select();
  }, [isRenaming]);

  const handleClick = () => {
    if (isDirectory) {
      setExpanded((value) => !value);
    } else {
      onFileSelect?.(entry.path);
    }
  };

  const openMenuAtPoint = (point: { readonly x: number; readonly y: number }) =>
    onOpenContextMenu(
      {
        entry,
        expanded,
        onOpen: handleClick,
        ...(isDirectory
          ? {
              onRefresh: () => {
                if (expanded) {
                  void refetch();
                }
              },
            }
          : {}),
      },
      point,
    );

  const rowPadding = `${depth * 12 + 12}px`;

  return (
    <div className="relative">
      {isRenaming ? (
        <form
          className="flex min-w-0 items-center gap-2 px-3 py-1"
          style={{ paddingLeft: rowPadding }}
          onSubmit={(event) => {
            event.preventDefault();
            onSubmitRename(entry);
          }}
          onContextMenu={(event) => {
            event.preventDefault();
          }}
        >
          <TreeChevron isDirectory={isDirectory} expanded={expanded} />
          <TreeIcon isDirectory={isDirectory} expanded={expanded} />
          <Input
            ref={inputRef}
            value={renameDraft}
            onChange={(event) => onRenameDraftChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                onCancelRename();
              }
            }}
            disabled={busy}
            aria-label="File name"
            className="h-7 min-w-0 px-2 py-1 text-sm"
          />
          <Tooltip content="Save" side="right">
            <button
              type="submit"
              aria-label="Save file name"
              disabled={busy || renameDraft.trim().length === 0}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-border-strong/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              <Check className="h-4 w-4" />
            </button>
          </Tooltip>
          <Tooltip content="Cancel" side="right">
            <button
              type="button"
              aria-label="Cancel file rename"
              disabled={busy}
              onClick={onCancelRename}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-border-strong/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              <X className="h-4 w-4" />
            </button>
          </Tooltip>
        </form>
      ) : (
        <button
          type="button"
          onClick={handleClick}
          onKeyDown={(event) => {
            if (
              event.key !== "ContextMenu" &&
              !(event.shiftKey && event.key === "F10")
            ) {
              return;
            }

            event.preventDefault();
            const rect = event.currentTarget.getBoundingClientRect();
            openMenuAtPoint({ x: rect.left + 24, y: rect.bottom });
          }}
          onContextMenu={(event) => {
            event.preventDefault();
            openMenuAtPoint({ x: event.clientX, y: event.clientY });
          }}
          aria-haspopup="menu"
          {...(isDirectory ? { "aria-expanded": expanded } : {})}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border-accent"
          style={{ paddingLeft: rowPadding }}
        >
          <TreeChevron isDirectory={isDirectory} expanded={expanded} />
          <TreeIcon isDirectory={isDirectory} expanded={expanded} />
          <span className="min-w-0 flex-1 truncate">{entry.name}</span>
          {entry.changed && (
            <span className="shrink-0 text-xs text-info">{entry.changed}</span>
          )}
        </button>
      )}

      {isDirectory && expanded && (
        <div>
          {isLoading && (
            <div
              className="py-1 text-xs text-muted-foreground"
              style={{ paddingLeft: `${(depth + 1) * 12 + 12}px` }}
            >
              Loading...
            </div>
          )}
          {childData?.entries.map((child) => (
            <FileTreeNode
              key={child.path}
              entry={child}
              projectId={projectId}
              depth={depth + 1}
              busy={busy}
              renamingPath={renamingPath}
              renameDraft={renameDraft}
              onFileSelect={onFileSelect}
              onRenameDraftChange={onRenameDraftChange}
              onSubmitRename={onSubmitRename}
              onCancelRename={onCancelRename}
              onOpenContextMenu={onOpenContextMenu}
            />
          ))}
          {!isLoading && !hasChildren && (
            <div
              className="py-1 text-xs text-muted-foreground"
              style={{ paddingLeft: `${(depth + 1) * 12 + 12}px` }}
            >
              Empty folder
            </div>
          )}
        </div>
      )}
    </div>
  );
}

interface FileTreeProps {
  readonly projectId: string;
  readonly entries: WorkspaceTreeEntry[];
  readonly onFileSelect?: ((path: string) => void) | undefined;
}

export function FileTree({ projectId, entries, onFileSelect }: FileTreeProps) {
  const renamePath = useRenameWorkspacePath(projectId);
  const deletePath = useDeleteWorkspacePath(projectId);
  const writeFile = useWriteWorkspaceFile(projectId);
  const rootRef = useRef<HTMLDivElement>(null);
  const [menu, setMenu] = useState<FileTreeMenuState | undefined>();
  const [renamingPath, setRenamingPath] = useState<string | undefined>();
  const [renameDraft, setRenameDraft] = useState("");
  const [undoAction, setUndoAction] = useState<
    UndoWorkspaceAction | undefined
  >();
  const [actionError, setActionError] = useState<string | undefined>();
  const busy =
    renamePath.isPending || deletePath.isPending || writeFile.isPending;

  const openContextMenu = useCallback(
    (
      nextMenu: Omit<FileTreeMenuState, "x" | "y">,
      point: { readonly x: number; readonly y: number },
    ) => {
      setMenu({
        ...nextMenu,
        ...clampMenuPoint(point),
      });
    },
    [],
  );

  const closeMenu = () => setMenu(undefined);

  const startRename = (entry: WorkspaceTreeEntry) => {
    closeMenu();
    setActionError(undefined);
    setRenamingPath(entry.path);
    setRenameDraft(entry.name);
  };

  const cancelRename = () => {
    setRenamingPath(undefined);
    setRenameDraft("");
  };

  const submitRename = async (entry: WorkspaceTreeEntry) => {
    const nextName = renameDraft.trim();

    if (nextName.length === 0) {
      return;
    }

    if (nextName === entry.name) {
      cancelRename();
      return;
    }

    const toPath = joinTreePath(parentTreePath(entry.path), nextName);
    await runWorkspaceMove({
      move: () => renamePath.mutateAsync({ fromPath: entry.path, toPath }),
      onSuccess: () => {
        setUndoAction({
          label: `Rename ${nextName}`,
          run: () =>
            renamePath.mutateAsync({ fromPath: toPath, toPath: entry.path }),
        });
        cancelRename();
      },
      onAfter: () => rootRef.current?.focus(),
      setActionError,
    });
  };

  const deleteEntry = async (entry: WorkspaceTreeEntry) => {
    closeMenu();
    const snapshot =
      entry.type === "file"
        ? await getWorkspaceFile(projectId, entry.path).catch(() => undefined)
        : undefined;
    await runWorkspaceMove({
      move: () => deletePath.mutateAsync({ path: entry.path }),
      onSuccess: () => {
        if (snapshot === undefined) {
          setUndoAction(undefined);
          return;
        }

        setUndoAction({
          label: `Delete ${entry.name}`,
          run: () =>
            writeFile.mutateAsync({
              path: entry.path,
              content: snapshot.file.content,
            }),
        });
      },
      onAfter: () => rootRef.current?.focus(),
      setActionError,
    });
  };

  const undoLastAction = async () => {
    if (undoAction === undefined || busy) {
      return;
    }

    const action = undoAction;
    await runWorkspaceMove({
      move: () => action.run(),
      onSuccess: () => setUndoAction(undefined),
      onAfter: () => rootRef.current?.focus(),
      setActionError,
    });
  };

  return (
    <div
      ref={rootRef}
      role="tree"
      tabIndex={-1}
      className="relative h-full min-h-0 overflow-y-auto focus-visible:outline-none"
      onKeyDown={(event) => {
        if (!isUndoKeyboardEvent(event) || isTextEditingTarget(event.target)) {
          return;
        }

        event.preventDefault();
        void undoLastAction();
      }}
      onContextMenu={(event) => {
        if (event.target === event.currentTarget) {
          event.preventDefault();
        }
      }}
    >
      {actionError === undefined ? null : (
        <div className="mx-2 mt-2 rounded-sm border border-destructive/30 bg-destructive-soft px-2 py-1.5 text-xs text-destructive">
          {actionError}
        </div>
      )}

      {entries.map((entry) => (
        <FileTreeNode
          key={entry.path}
          entry={entry}
          projectId={projectId}
          depth={0}
          busy={busy}
          renamingPath={renamingPath}
          renameDraft={renameDraft}
          onFileSelect={onFileSelect}
          onRenameDraftChange={setRenameDraft}
          onSubmitRename={submitRename}
          onCancelRename={cancelRename}
          onOpenContextMenu={openContextMenu}
        />
      ))}

      <FileTreeContextMenu
        menu={menu}
        busy={busy}
        canUndo={undoAction !== undefined}
        onClose={closeMenu}
        onStartRename={startRename}
        onDelete={deleteEntry}
        onUndo={() => void undoLastAction()}
        onCopyText={(value) => void navigator.clipboard?.writeText(value)}
      />
    </div>
  );
}

function FileTreeContextMenu({
  menu,
  busy,
  canUndo,
  onClose,
  onStartRename,
  onDelete,
  onUndo,
  onCopyText,
}: {
  readonly menu: FileTreeMenuState | undefined;
  readonly busy: boolean;
  readonly canUndo: boolean;
  readonly onClose: () => void;
  readonly onStartRename: (entry: WorkspaceTreeEntry) => void;
  readonly onDelete: (entry: WorkspaceTreeEntry) => void;
  readonly onUndo: () => void;
  readonly onCopyText: (value: string) => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (menu === undefined) {
      return;
    }

    const handlePointerDown = (event: PointerEvent) => {
      if (
        event.target instanceof Node &&
        menuRef.current?.contains(event.target)
      ) {
        return;
      }

      onClose();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };

    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [menu, onClose]);

  if (menu === undefined) {
    return null;
  }

  const entry = menu.entry;
  const isDirectory = entry.type === "dir";
  const runItem = (action: () => void) => {
    action();
    onClose();
  };

  const content = (
    <div
      ref={menuRef}
      role="menu"
      className="fixed z-50 w-56 rounded-md border border-border bg-card p-1 text-foreground shadow-md"
      style={{ left: menu.x, top: menu.y }}
    >
      <ContextMenuItem
        icon={
          isDirectory ? (
            menu.expanded ? (
              <ChevronDown className="h-4 w-4" />
            ) : (
              <ChevronRight className="h-4 w-4" />
            )
          ) : (
            <File className="h-4 w-4" />
          )
        }
        onClick={() => runItem(menu.onOpen)}
      >
        {isDirectory ? (menu.expanded ? "Collapse" : "Expand") : "Open"}
      </ContextMenuItem>
      <ContextMenuSeparator />
      <ContextMenuItem
        icon={<Copy className="h-4 w-4" />}
        onClick={() => runItem(() => onCopyText(entry.path))}
      >
        Copy path
      </ContextMenuItem>
      <ContextMenuItem
        icon={<Copy className="h-4 w-4" />}
        onClick={() => runItem(() => onCopyText(entry.name))}
      >
        Copy name
      </ContextMenuItem>
      <ContextMenuSeparator />
      <ContextMenuItem
        icon={<Pencil className="h-4 w-4" />}
        disabled={busy}
        onClick={() => runItem(() => onStartRename(entry))}
      >
        Rename
      </ContextMenuItem>
      <ContextMenuItem
        icon={<Trash2 className="h-4 w-4" />}
        disabled={busy}
        onClick={() => runItem(() => void onDelete(entry))}
      >
        Delete
      </ContextMenuItem>
      <ContextMenuItem
        icon={<Undo2 className="h-4 w-4" />}
        disabled={busy || !canUndo}
        onClick={() => runItem(onUndo)}
      >
        Undo
      </ContextMenuItem>
      {menu.onRefresh === undefined ? null : (
        <>
          <ContextMenuSeparator />
          <ContextMenuItem
            icon={<RefreshCw className="h-4 w-4" />}
            disabled={!menu.expanded}
            onClick={() => runItem(menu.onRefresh ?? (() => undefined))}
          >
            Refresh folder
          </ContextMenuItem>
        </>
      )}
    </div>
  );

  return createPortal(content, document.body);
}

function ContextMenuItem({
  icon,
  disabled,
  onClick,
  children,
}: {
  readonly icon: ReactNode;
  readonly disabled?: boolean | undefined;
  readonly onClick: () => void;
  readonly children: ReactNode;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      onClick={onClick}
      className="flex w-full cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-muted focus:bg-muted disabled:pointer-events-none disabled:opacity-50"
    >
      <span className="flex h-4 w-4 items-center justify-center">{icon}</span>
      {children}
    </button>
  );
}

function ContextMenuSeparator() {
  return <hr className="-mx-1 my-1 border-border" />;
}

function TreeChevron({
  isDirectory,
  expanded,
}: {
  readonly isDirectory: boolean;
  readonly expanded: boolean;
}) {
  if (!isDirectory) {
    return <span className="w-4 shrink-0" />;
  }

  return (
    <span className="shrink-0 text-muted-foreground">
      {expanded ? (
        <ChevronDown className="h-4 w-4" />
      ) : (
        <ChevronRight className="h-4 w-4" />
      )}
    </span>
  );
}

function TreeIcon({
  isDirectory,
  expanded,
}: {
  readonly isDirectory: boolean;
  readonly expanded: boolean;
}) {
  return (
    <span className="shrink-0 text-muted-foreground">
      {isDirectory ? (
        expanded ? (
          <FolderOpen className="h-4 w-4" />
        ) : (
          <Folder className="h-4 w-4" />
        )
      ) : (
        <File className="h-4 w-4" />
      )}
    </span>
  );
}

async function runWorkspaceMove({
  move,
  onSuccess,
  onAfter,
  setActionError,
}: {
  readonly move: () => Promise<unknown>;
  readonly onSuccess: () => void;
  readonly onAfter: () => void;
  readonly setActionError: (message: string | undefined) => void;
}) {
  try {
    setActionError(undefined);
    await move();
    onSuccess();
  } catch (error) {
    setActionError(errorMessage(error));
  } finally {
    onAfter();
  }
}

function clampMenuPoint(point: {
  readonly x: number;
  readonly y: number;
}): Pick<FileTreeMenuState, "x" | "y"> {
  return {
    x: Math.max(8, Math.min(point.x, window.innerWidth - contextMenuWidth - 8)),
    y: Math.max(
      8,
      Math.min(point.y, window.innerHeight - contextMenuHeight - 8),
    ),
  };
}

function isUndoKeyboardEvent(event: React.KeyboardEvent): boolean {
  return (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z";
}

function isTextEditingTarget(target: EventTarget): boolean {
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : "File action failed.";
}

function parentTreePath(path: string): string {
  const parts = path.split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}

function joinTreePath(parentPath: string, name: string): string {
  return parentPath.length === 0 ? name : `${parentPath}/${name}`;
}
