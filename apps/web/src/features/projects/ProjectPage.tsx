import { A, useParams } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import FileCode2 from "lucide-solid/icons/file-code-2";
import Files from "lucide-solid/icons/files";
import GitBranch from "lucide-solid/icons/git-branch";
import GitCompare from "lucide-solid/icons/git-compare";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import X from "lucide-solid/icons/x";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { Badge } from "../../components/ui/Badge";
import { BootSplash } from "../../components/ui/BootSplash";
import { EmptyState } from "../../components/ui/EmptyState";
import { ErrorBlock } from "../../components/ui/ErrorBlock";
import type { FileMetaView } from "../../lib/api";
import { useProject } from "../../lib/queries";
import { FileEditor } from "./workspace/FileEditor";
import { FileTreePanel } from "./workspace/FileTreePanel";
import { ScmPanel } from "./workspace/ScmPanel";
import { basename } from "./workspace/utils";

/**
 * ProjectPage owns the main-area tab model. The main area is always the tab
 * strip + a single surface; there is no "graph mode" and no surface gets
 * painted over another. Graph history lives inside the Source Control side
 * panel (inline list + hover details), never as a main-area tab.
 */

export interface FileTab {
  id: string;
  kind: "file";
  path: string;
  draft: string;
  saved: string;
  meta: FileMetaView | null;
  loadError: string;
  loading: boolean;
}

export type MainTab = FileTab;

type ActivityView = "explorer" | "scm" | "terminal";

let tabIdSeq = 0;
function nextTabId(): string {
  tabIdSeq += 1;
  return `tab-${tabIdSeq}`;
}

export function ProjectPage() {
  const params = useParams<{ id: string }>();
  const projectId = () => params.id;
  const project = useProject(projectId);

  // --- Activity (switcher) rail state: only Explorer / Source Control / Terminal. ---
  const [activeView, setActiveView] = createSignal<ActivityView>("explorer");
  const [sidebarOpen, setSidebarOpen] = createSignal(true);

  // --- Main-area tab model: the sole owner of what the main area renders. ---
  const [tabs, setTabs] = createStore<MainTab[]>([]);
  const [activeTabId, setActiveTabId] = createSignal<string | null>(null);

  const activeTab = createMemo(() => {
    const id = activeTabId();
    if (!id) return undefined;
    return tabs.find((tab) => tab.id === id);
  });
  const activeFilePath = createMemo(() => activeTab()?.path ?? null);

  let treeRefreshToken = 0;
  const [treeRefresh, setTreeRefresh] = createSignal(treeRefreshToken);
  let lastMainRevision: string | null | undefined;
  let trackedProjectId: string | undefined;

  function selectActivity(view: ActivityView) {
    if (activeView() === view) {
      setSidebarOpen((open) => !open);
      return;
    }
    setActiveView(view);
    setSidebarOpen(true);
  }

  function openFile(path: string) {
    const existing = tabs.find((tab) => tab.path === path);
    if (existing) {
      setActiveTabId(existing.id);
      return;
    }
    const id = nextTabId();
    setTabs(tabs.length, {
      id,
      kind: "file",
      path,
      draft: "",
      saved: "",
      meta: null,
      loadError: "",
      loading: false,
    });
    setActiveTabId(id);
  }

  function patchFileTab(id: string, mutator: (tab: FileTab) => void) {
    setTabs((list) =>
      list.map((tab) => {
        if (tab.id !== id) return tab;
        const next: FileTab = { ...tab };
        mutator(next);
        return next;
      }),
    );
  }

  function closeTab(id: string) {
    const idx = tabs.findIndex((tab) => tab.id === id);
    if (idx < 0) return;
    const wasActive = activeTabId() === id;
    const neighbor = tabs[idx + 1] ?? tabs[idx - 1] ?? null;
    setTabs((list) => list.filter((tab) => tab.id !== id));
    if (wasActive) setActiveTabId(neighbor ? neighbor.id : null);
  }

  async function handleSaved(_projectId: string) {
    treeRefreshToken += 1;
    setTreeRefresh(treeRefreshToken);
  }

  // Reset local IDE state when navigating between projects.
  createEffect(() => {
    const id = projectId();
    if (!id) return;
    if (trackedProjectId !== undefined && trackedProjectId !== id) {
      setTabs([]);
      setActiveTabId(null);
      setActiveView("explorer");
      setSidebarOpen(true);
      treeRefreshToken = 0;
      setTreeRefresh(0);
      lastMainRevision = undefined;
    }
    trackedProjectId = id;
  });

  // Keep the lazy tree aligned with SSE / multi-client main revision bumps.
  createEffect(() => {
    const id = projectId();
    const revision = project.data?.main_revision ?? null;
    if (!id) {
      lastMainRevision = undefined;
      return;
    }
    if (revision === null) {
      lastMainRevision = revision;
      return;
    }
    if (lastMainRevision !== undefined && lastMainRevision !== revision) {
      setTreeRefresh((n) => n + 1);
    }
    lastMainRevision = revision;
  });

  const branch = () => project.data?.current_branch ?? project.data?.repository.branch ?? null;

  return (
    <section class="project-page project-page--ide route-enter" aria-labelledby="project-title">
      <header class="workspace-topbar">
        <A class="project-back" href="/">
          <ArrowLeft size={16} />
          Exit
        </A>
        <Show
          when={project.data}
          fallback={
            <Show
              when={project.isError}
              fallback={
                <div class="workspace-title-row" aria-busy="true">
                  <div class="workspace-identity">
                    <div class="workspace-name">
                      <span>Workspace:</span>
                      <h1 id="project-title">...</h1>
                    </div>
                  </div>
                </div>
              }
            >
              <ErrorBlock
                message={
                  project.error instanceof Error ? project.error.message : "Project not found"
                }
                retry={() => void project.refetch()}
              />
            </Show>
          }
        >
          {(data) => (
            <div class="workspace-title-row">
              <div class="workspace-identity">
                <div class="workspace-name">
                  <span>Workspace:</span>
                  <h1 id="project-title">{data().name}</h1>
                </div>
                <p class="project-subtitle">
                  {data().repository.url}
                  <Show when={data().current_branch ?? data().repository.branch}>
                    {(b) => (
                      <>
                        {" · "}
                        <GitBranch size={12} class="inline-icon" /> {b()}
                      </>
                    )}
                  </Show>
                </p>
              </div>
              <Badge
                variant={
                  data().state === "ready"
                    ? "success"
                    : data().state === "error"
                      ? "danger"
                      : "warning"
                }
              >
                {data().state}
              </Badge>
            </div>
          )}
        </Show>
      </header>

      <Show
        when={project.data?.state === "ready"}
        fallback={
          <Show when={project.data} fallback={<BootSplash />}>
            {(data) => (
              <EmptyState
                icon={Files}
                title={`Project is ${data().state}`}
                description={
                  data().state === "creating"
                    ? "Clone is still running. Files and Git unlock when the project is ready."
                    : "This project is not ready for workspace tools yet."
                }
              />
            )}
          </Show>
        }
      >
        <div class="ide-shell" classList={{ "ide-shell--sidebar-collapsed": !sidebarOpen() }}>
          <nav class="ide-activity-bar" aria-label="Workspace activity">
            <ActivityButton
              label="Explorer"
              active={activeView() === "explorer" && sidebarOpen()}
              onClick={() => selectActivity("explorer")}
            >
              <Files size={18} />
            </ActivityButton>
            <ActivityButton
              label="Source Control"
              active={activeView() === "scm" && sidebarOpen()}
              onClick={() => selectActivity("scm")}
            >
              <GitCompare size={18} />
            </ActivityButton>
            <ActivityButton
              label="Terminal"
              active={activeView() === "terminal" && sidebarOpen()}
              onClick={() => selectActivity("terminal")}
            >
              <TerminalSquare size={18} />
            </ActivityButton>
          </nav>

          {/*
            Keep the sidebar chrome mounted while open so activity switches only
            swap panel content, not the whole rail. That avoids a layout flash.
          */}
          <Show when={sidebarOpen()}>
            <aside class="ide-sidebar" aria-label="Workspace sidebar">
              <div
                class="ide-sidebar-view"
                classList={{ "ide-sidebar-view--active": activeView() === "explorer" }}
                hidden={activeView() !== "explorer"}
              >
                <FileTreePanel
                  projectId={projectId}
                  activePath={activeFilePath}
                  onOpenFile={openFile}
                  refreshToken={treeRefresh}
                />
              </div>
              <div
                class="ide-sidebar-view"
                classList={{ "ide-sidebar-view--active": activeView() === "scm" }}
                hidden={activeView() !== "scm"}
              >
                <ScmPanel projectId={projectId} onOpenFile={openFile} branch={branch} />
              </div>
              <div
                class="ide-sidebar-view"
                classList={{ "ide-sidebar-view--active": activeView() === "terminal" }}
                hidden={activeView() !== "terminal"}
              >
                <div class="ide-sidebar-panel">
                  <div class="ide-sidebar-header">Terminal</div>
                  <EmptyState
                    icon={TerminalSquare}
                    title="Terminal unavailable"
                    description="Main Workspace Terminal depends on RuntimeExecutor and lands in M4. This panel is a placeholder."
                    class="terminal-placeholder"
                  />
                </div>
              </div>
            </aside>
          </Show>

          {/*
            Main area is always owned by the tab strip. Files are the only tab
            kind; Graph lives in the Source Control side panel. Nothing paints
            over the editor or hides the tab strip.
          */}
          <main class="ide-main">
            <Show when={tabs.length > 0}>
              <div class="ide-main-tabs">
                <div class="ide-tabs" role="tablist" aria-label="Open editors">
                  <For each={tabs}>
                    {(tab) => (
                      <TabView
                        tab={tab}
                        active={activeTabId() === tab.id}
                        onActivate={() => setActiveTabId(tab.id)}
                        onClose={() => closeTab(tab.id)}
                      />
                    )}
                  </For>
                </div>
              </div>
            </Show>
            <div class="ide-main-surface">
              <Show
                when={activeTab()}
                fallback={
                  <EmptyState
                    icon={FileCode2}
                    title="No file open"
                    description="Open a file from the Explorer. Commit history lives under Source Control."
                  />
                }
              >
                {(tab) => (
                  <FileEditor
                    projectId={projectId}
                    mainRevision={() => project.data?.main_revision ?? null}
                    tab={() => tab() as FileTab}
                    onPatch={(mutator) => patchFileTab(tab().id, mutator)}
                    onSaved={handleSaved}
                  />
                )}
              </Show>
            </div>
          </main>
        </div>
      </Show>
    </section>
  );
}

function TabView(props: {
  tab: FileTab;
  active: boolean;
  onActivate: () => void;
  onClose: () => void;
}) {
  const dirty = () => Boolean(props.tab.meta?.editable) && props.tab.draft !== props.tab.saved;
  return (
    <div class="ide-tab" classList={{ "ide-tab--active": props.active, "ide-tab--dirty": dirty() }}>
      <button
        type="button"
        class="ide-tab-label"
        role="tab"
        aria-selected={props.active}
        aria-label={`${basename(props.tab.path)}${dirty() ? ", unsaved changes" : ""}`}
        title={props.tab.path}
        onClick={props.onActivate}
      >
        <Show when={dirty()}>
          <span class="ide-tab-dirty" aria-hidden="true" title="Unsaved changes" />
        </Show>
        <span>{basename(props.tab.path)}</span>
      </button>
      <button
        type="button"
        class="ide-tab-close"
        aria-label={`Close ${basename(props.tab.path)}`}
        onClick={(event) => {
          event.stopPropagation();
          props.onClose();
        }}
      >
        <X size={12} />
      </button>
    </div>
  );
}

function ActivityButton(props: {
  label: string;
  active: boolean;
  onClick: () => void;
  children: import("solid-js").JSX.Element;
}) {
  return (
    <button
      type="button"
      class="ide-activity-btn"
      classList={{ "ide-activity-btn--active": props.active }}
      aria-label={props.label}
      aria-pressed={props.active}
      title={props.label}
      onClick={props.onClick}
    >
      {props.children}
      <span>{props.label}</span>
    </button>
  );
}
