import ChevronRight from "lucide-solid/icons/chevron-right";
import FileCode2 from "lucide-solid/icons/file-code-2";
import { createEffect, createMemo, createSignal, For, onCleanup, Show, untrack } from "solid-js";
import { NotificationEvent } from "../../components/ui/notifications";
import { SideScrollbar } from "../../components/ui/SideScrollbar";
import type { FileTreeView } from "../../lib/api";
import { getErrorMessage, listFileTree } from "../../lib/api";
import { basename, sortTreeEntries } from "../../lib/utils";
import "./files.css";

interface FileTreePanelProps {
  projectId: () => string | undefined;
  activePath: () => string | null;
  onOpenFile: (path: string) => void;
  /** Bump to drop cached children (e.g. after save / SSE). */
  refreshToken?: () => number;
}

export function FileTreePanel(props: FileTreePanelProps) {
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set([""]));
  const [children, setChildren] = createSignal<Record<string, FileTreeView[]>>({});
  const [loading, setLoading] = createSignal<Set<string>>(new Set());
  const [errors, setErrors] = createSignal<Record<string, string>>({});
  const [scrollHost, setScrollHost] = createSignal<HTMLElement | null>(null);
  const [focusedPath, setFocusedPath] = createSignal<string | null>(null);
  let treeEl: HTMLDivElement | undefined;
  let prefetchTimer: ReturnType<typeof setTimeout> | undefined;

  async function loadPath(path: string, force = false) {
    const id = props.projectId();
    if (!id) return;
    if (!force && children()[path] !== undefined && !errors()[path]) return;

    setLoading((current) => new Set(current).add(path));
    setErrors((current) => {
      const next = { ...current };
      delete next[path];
      return next;
    });
    try {
      const entries = sortTreeEntries(await listFileTree(id, path || undefined));
      setChildren((current) => ({ ...current, [path]: entries }));
    } catch (error) {
      setErrors((current) => ({
        ...current,
        [path]: getErrorMessage(error, "Failed to load directory"),
      }));
    } finally {
      setLoading((current) => {
        const next = new Set(current);
        next.delete(path);
        return next;
      });
    }
  }

  createEffect(() => {
    const id = props.projectId();
    if (!id) return;
    // Re-run on save / revision refresh without collapsing the tree.
    props.refreshToken?.();
    setChildren({});
    setErrors({});
    const paths = untrack(() => {
      const open = [...expanded()];
      return open.includes("") ? open : ["", ...open];
    });
    for (const path of paths) {
      void loadPath(path, true);
    }
  });

  // Background prefetch of depth-2 directories: once the root listing lands and
  // the tree is rendered, quietly pull each top-level directory's children so
  // that expanding a top-level folder is effectively instant (hit cache) instead
  // of waiting on a first network round-trip. No UI flicker — this only fills the
  // cache; the rows don't mount until the user expands. Guarded so it never
  // re-fetches a dir already cached or in flight.
  const activeId = createMemo(() => props.projectId());
  createEffect(() => {
    const id = activeId();
    // depend on refreshToken so a project save/re-revision re-prefetches the
    // fresh tree; rootKey diffs to avoid redundant work across rapid refreshes.
    props.refreshToken?.();
    const rootEntries = children()[""];
    if (!id || !rootEntries) return;
    prefetchTimer = setTimeout(() => {
      for (const entry of rootEntries) {
        if (
          entry.kind === "dir" &&
          children()[entry.path] === undefined &&
          !loading().has(entry.path)
        ) {
          void loadPath(entry.path);
        }
      }
    }, 150);
    onCleanup(() => {
      if (prefetchTimer !== undefined) clearTimeout(prefetchTimer);
      prefetchTimer = undefined;
    });
  });

  function toggleDir(path: string) {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else {
        next.add(path);
        void loadPath(path);
      }
      return next;
    });
  }

  // Flattened list of the rows a sighted user can actually see, in render order.
  // Arrow-key navigation and the roving tabindex both walk this list.
  const visibleRows = createMemo(() => {
    const rows: { path: string; kind: string }[] = [];
    const walk = (parent: string) => {
      for (const entry of children()[parent] ?? []) {
        rows.push({ path: entry.path, kind: entry.kind });
        if (entry.kind === "dir" && expanded().has(entry.path)) walk(entry.path);
      }
    };
    walk("");
    return rows;
  });

  // Collapsing a parent can hide the row that last held focus, so fall back to
  // the first row instead of leaving the tree without a tab stop.
  const activeRow = createMemo(() => {
    const rows = visibleRows();
    const current = focusedPath();
    if (current && rows.some((row) => row.path === current)) return current;
    return rows[0]?.path;
  });

  function focusRow(path: string | undefined) {
    if (!path) return;
    setFocusedPath(path);
    const rows = treeEl?.querySelectorAll<HTMLElement>("[data-tree-path]");
    if (!rows) return;
    for (const row of rows) {
      if (row.dataset.treePath === path) {
        row.focus();
        return;
      }
    }
  }

  function onTreeKeyDown(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    // Let the inline Retry button keep its own Enter/Space handling.
    if (!target?.dataset.treePath) return;
    const rows = visibleRows();
    if (rows.length === 0) return;
    const index = rows.findIndex((row) => row.path === activeRow());
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusRow(rows[Math.min(index + 1, rows.length - 1)]?.path);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      focusRow(rows[Math.max(index - 1, 0)]?.path);
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      focusRow(rows[0]?.path);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      focusRow(rows[rows.length - 1]?.path);
      return;
    }
    const row = rows[index];
    if (!row) return;
    if (event.key === "ArrowRight") {
      event.preventDefault();
      if (row.kind !== "dir") return;
      if (expanded().has(row.path)) focusRow(rows[index + 1]?.path);
      else toggleDir(row.path);
      return;
    }
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      if (row.kind === "dir" && expanded().has(row.path)) {
        toggleDir(row.path);
        return;
      }
      const separator = row.path.lastIndexOf("/");
      if (separator > 0) focusRow(row.path.slice(0, separator));
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (row.kind === "dir") toggleDir(row.path);
      else props.onOpenFile(row.path);
    }
  }

  return (
    <section class="ide-sidebar-panel" aria-label="Explorer">
      <NotificationEvent
        message={errors()[""]}
        variant="danger"
        action={{ label: "Retry", onClick: () => void loadPath("", true) }}
      />
      <div class="ide-sidebar-header">Explorer</div>
      <div class="ide-scroll-host">
        <div class="ide-tree ide-sidebar-scroll" ref={setScrollHost}>
          <Show
            when={children()[""] !== undefined}
            fallback={
              <Show when={!errors()[""]}>
                <p class="surface-note" role="status">
                  Loading…
                </p>
              </Show>
            }
          >
            <Show
              when={(children()[""]?.length ?? 0) > 0}
              fallback={<p class="surface-note">Empty repository</p>}
            >
              {/* Plain divs rather than a list: an ARIA tree replaces list
                  semantics, so a list element's implicit role only conflicts
                  with role="tree". Depth is carried by aria-level per row. */}
              <div
                ref={treeEl}
                class="ide-tree-list"
                role="tree"
                aria-label="Files"
                tabIndex={-1}
                onKeyDown={onTreeKeyDown}
              >
                <For each={children()[""] ?? []}>
                  {(entry) => (
                    <TreeNode
                      entry={entry}
                      depth={0}
                      expanded={expanded()}
                      childrenMap={children()}
                      loading={loading()}
                      errors={errors()}
                      activePath={props.activePath()}
                      focusedPath={activeRow()}
                      onToggle={toggleDir}
                      onOpenFile={props.onOpenFile}
                      onFocusRow={focusRow}
                      onRetry={(path) => void loadPath(path, true)}
                    />
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </div>
        <SideScrollbar host={scrollHost} />
      </div>
    </section>
  );
}

function TreeNode(props: {
  entry: FileTreeView;
  depth: number;
  expanded: Set<string>;
  childrenMap: Record<string, FileTreeView[]>;
  loading: Set<string>;
  errors: Record<string, string>;
  activePath: string | null;
  focusedPath: string | undefined;
  onToggle: (path: string) => void;
  onOpenFile: (path: string) => void;
  onFocusRow: (path: string) => void;
  onRetry: (path: string) => void;
}) {
  const isDir = () => props.entry.kind === "dir";
  const isOpen = () => props.expanded.has(props.entry.path);
  const isActiveFile = () => !isDir() && props.activePath === props.entry.path;
  const pad = () => ({ "padding-left": `${8 + props.depth * 12}px` });
  const childEntries = () => props.childrenMap[props.entry.path];
  const isLoading = () =>
    props.loading.has(props.entry.path) || (isOpen() && childEntries() === undefined);

  return (
    <div
      class="ide-tree-node"
      role="treeitem"
      data-tree-path={props.entry.path}
      tabIndex={props.focusedPath === props.entry.path ? 0 : -1}
      aria-level={props.depth + 1}
      aria-expanded={isDir() ? isOpen() : undefined}
      aria-selected={isDir() ? undefined : isActiveFile()}
    >
      <button
        type="button"
        class="ide-tree-item"
        classList={{
          "ide-tree-item--active": isActiveFile(),
          "ide-tree-item--dir": isDir(),
        }}
        style={pad()}
        tabIndex={-1}
        onClick={() => {
          props.onFocusRow(props.entry.path);
          if (isDir()) props.onToggle(props.entry.path);
          else props.onOpenFile(props.entry.path);
        }}
      >
        {/* Fixed-width icon slot so directories (chevron only) and files
            (file glyph) align on the same left edge — no folder glyph anymore,
            the > / v chevron alone signals a directory. */}
        <span class="ide-tree-icon-slot">
          <Show when={isDir()} fallback={<FileCode2 size={14} class="ide-tree-icon" />}>
            <span
              class="ide-tree-chevron"
              classList={{ "ide-tree-chevron--open": isOpen() }}
              aria-hidden="true"
            >
              <ChevronRight size={12} />
            </span>
          </Show>
        </span>
        <span class="ide-tree-label">{basename(props.entry.path)}</span>
      </button>
      <Show when={isDir()}>
        <div
          class="ide-tree-children"
          classList={{ "ide-tree-children--open": isOpen() }}
          aria-hidden={!isOpen()}
        >
          <div class="ide-tree-children-inner">
            <Show
              when={!isLoading()}
              fallback={
                <div class="ide-tree-loading" style={pad()}>
                  Loading…
                </div>
              }
            >
              <Show
                when={!props.errors[props.entry.path]}
                fallback={
                  <div class="ide-tree-loading ide-tree-error" style={pad()}>
                    <span title={props.errors[props.entry.path] ?? ""}>
                      {props.errors[props.entry.path]}
                    </span>
                    <button
                      type="button"
                      class="ide-tree-retry"
                      aria-label={`Retry loading ${basename(props.entry.path)}`}
                      onClick={() => props.onRetry(props.entry.path)}
                    >
                      Retry
                    </button>
                  </div>
                }
              >
                <Show
                  when={(childEntries()?.length ?? 0) > 0}
                  fallback={
                    <div class="ide-tree-loading" style={pad()}>
                      Empty
                    </div>
                  }
                >
                  <div class="ide-tree-list">
                    <For each={childEntries() ?? []}>
                      {(child) => (
                        <TreeNode
                          entry={child}
                          depth={props.depth + 1}
                          expanded={props.expanded}
                          childrenMap={props.childrenMap}
                          loading={props.loading}
                          errors={props.errors}
                          activePath={props.activePath}
                          focusedPath={props.focusedPath}
                          onToggle={props.onToggle}
                          onOpenFile={props.onOpenFile}
                          onFocusRow={props.onFocusRow}
                          onRetry={props.onRetry}
                        />
                      )}
                    </For>
                  </div>
                </Show>
              </Show>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}
