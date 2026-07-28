import { A, useParams } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import FileCode2 from "lucide-solid/icons/file-code-2";
import Files from "lucide-solid/icons/files";
import GitBranch from "lucide-solid/icons/git-branch";
import GitCompare from "lucide-solid/icons/git-compare";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import X from "lucide-solid/icons/x";
// TerminalSquare still used by the activity rail icon.
import { createEffect, createMemo, createSignal, For, Show, Suspense } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { Badge } from "../../components/ui/Badge";
import { EmptyState } from "../../components/ui/EmptyState";
import { ErrorBlock } from "../../components/ui/ErrorBlock";
import type { FileMetaView } from "../../lib/api";
import { useProject } from "../../lib/queries";
import { FileEditor } from "./workspace/FileEditor";
import { FileTreePanel } from "./workspace/FileTreePanel";
import { LazyTerminalPanel } from "./workspace/LazyTerminalPanel";
import { ScmPanel } from "./workspace/ScmPanel";
import { SessionsPanel } from "./workspace/SessionsPanel";
import { SessionTabView } from "./workspace/SessionTabView";
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

export interface SessionTab {
  id: string;
  kind: "session";
  sessionId: string;
  title: string;
  /** UX-SES-02: Main / Diff sub-view selection is UI state on the tab. */
  subView: "main" | "diff";
}

export type MainTab = FileTab | SessionTab;

type ActivityView = "explorer" | "sessions" | "scm" | "terminal";

let tabIdSeq = 0;
function nextTabId(): string {
  tabIdSeq += 1;
  return `tab-${tabIdSeq}`;
}

export function ProjectPage() {
  const params = useParams<{ id: string }>();
  const projectId = () => params.id;
  const project = useProject(projectId);

  // --- Activity rail: Sessions / Explorer / Source Control / Terminal. ---
  // Sessions sits first (matches the legacy Janus rail) and is the default
  // selection. This is local UI state only — deliberately NOT mirrored into
  // the URL. The previous design pushed ?view=sessions on every switch, which
  // made the Sessions panel read like a separate route from the rest of the
  // workspace; it is just a sidebar provider, like Explorer or SCM, so it
  // shares the same (URL-less) toggle model as they do.
  const [activeView, setActiveView] = createSignal<ActivityView>("sessions");
  const [sidebarOpen, setSidebarOpen] = createSignal(true);

  // --- Main-area tab model: the sole owner of what the main area renders. ---
  const [tabs, setTabs] = createStore<MainTab[]>([]);
  const [activeTabId, setActiveTabId] = createSignal<string | null>(null);

  const activeTab = createMemo(() => {
    const id = activeTabId();
    if (!id) return undefined;
    return tabs.find((tab) => tab.id === id);
  });
  const activeFilePath = createMemo(() => {
    const tab = activeTab();
    return tab?.kind === "file" ? tab.path : null;
  });
  const activeSessionId = createMemo(() => {
    const tab = activeTab();
    return tab?.kind === "session" ? tab.sessionId : null;
  });

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
    const existing = tabs.find((tab) => tab.kind === "file" && tab.path === path);
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

  function openSession(sessionId: string, title?: string | null) {
    const existing = tabs.find((tab) => tab.kind === "session" && tab.sessionId === sessionId);
    if (existing) {
      setActiveTabId(existing.id);
      setActiveView("sessions");
      setSidebarOpen(true);
      return;
    }
    const id = nextTabId();
    setTabs(tabs.length, {
      id,
      kind: "session",
      sessionId,
      title: title?.trim() || "New session",
      subView: "main",
    } satisfies SessionTab);
    setActiveTabId(id);
    setActiveView("sessions");
    setSidebarOpen(true);
  }

  /** Drop any open tab whose underlying session was deleted from the panel. */
  function closeSessionTabs(sessionId: string) {
    const remaining = tabs.filter(
      (tab) => !(tab.kind === "session" && tab.sessionId === sessionId),
    );
    if (remaining.length === tabs.length) return;
    const droppedActivating =
      activeTabId() !== null &&
      tabs.some(
        (tab) => tab.id === activeTabId() && tab.kind === "session" && tab.sessionId === sessionId,
      );
    setTabs(remaining);
    if (droppedActivating) {
      const neighbor = remaining[remaining.length - 1] ?? null;
      setActiveTabId(neighbor ? neighbor.id : null);
    }
  }

  function patchFileTab(id: string, mutator: (tab: FileTab) => void) {
    setTabs(
      produce((list) => {
        const tab = list.find((t) => t.id === id);
        if (tab && tab.kind === "file") mutator(tab);
      }),
    );
  }

  function patchSessionTab(id: string, mutator: (tab: SessionTab) => void) {
    setTabs(
      produce((list) => {
        const tab = list.find((t) => t.id === id);
        if (tab && tab.kind === "session") mutator(tab);
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
      setActiveView("sessions");
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
  // The IDE shell (activity rail, sidebar, tab strip, main surface) is always
  // mounted for the project route — the workspace scaffolding must not vanish
  // while a query is on the wire or the project is briefly non-ready. Only the
  // main-area *content* reflects project readiness; the chrome stays put.
  const ready = () => project.data?.state === "ready";

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

      <div class="ide-shell" classList={{ "ide-shell--sidebar-collapsed": !sidebarOpen() }}>
        <nav class="ide-activity-bar" aria-label="Workspace activity">
          <ActivityButton
            label="Sessions"
            active={activeView() === "sessions" && sidebarOpen()}
            disabled={!ready()}
            onClick={() => selectActivity("sessions")}
          >
            <MessageSquare size={18} />
          </ActivityButton>
          <ActivityButton
            label="Explorer"
            active={activeView() === "explorer" && sidebarOpen()}
            disabled={!ready()}
            onClick={() => selectActivity("explorer")}
          >
            <Files size={18} />
          </ActivityButton>
          <ActivityButton
            label="Source Control"
            active={activeView() === "scm" && sidebarOpen()}
            disabled={!ready()}
            onClick={() => selectActivity("scm")}
          >
            <GitCompare size={18} />
          </ActivityButton>
          <ActivityButton
            label="Terminal"
            active={activeView() === "terminal" && sidebarOpen()}
            disabled={!ready()}
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
              classList={{ "ide-sidebar-view--active": activeView() === "sessions" }}
              hidden={activeView() !== "sessions"}
            >
              <SessionsPanel
                projectId={projectId}
                projectReady={ready}
                activeSessionId={activeSessionId}
                onOpenSession={openSession}
                onSessionDeleted={closeSessionTabs}
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
              <div class="ide-sidebar-panel terminal-sidebar-panel">
                <Show when={activeView() === "terminal"}>
                  <LazyTerminalPanel
                    projectId={projectId}
                    ownerKind="project"
                    ownerId={projectId}
                    title="Main Terminal"
                    active={() => activeView() === "terminal"}
                  />
                </Show>
              </div>
            </div>
          </aside>
        </Show>

        {/*
            Main area is owned by the tab strip. Tabs can be Main-workspace files
            or Sessions (UX-SHELL-01). Graph stays in SCM side panel.
          */}
        <main class="ide-main">
          <Show when={tabs.length > 0}>
            <div class="ide-main-tabs">
              <div class="ide-tabs" role="tablist" aria-label="Open documents">
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
              when={ready() && activeTab()}
              fallback={
                <Show
                  when={ready()}
                  fallback={
                    <Show
                      when={project.data}
                      fallback={
                        <EmptyState
                          icon={Files}
                          title="Opening workspace…"
                          description="Connecting to the project."
                        />
                      }
                    >
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
                  <EmptyState
                    icon={FileCode2}
                    title="No document open"
                    description="Open a Session from the Sessions rail, or a file from Explorer."
                  />
                </Show>
              }
            >
              {(tab) => (
                <Suspense
                  fallback={
                    <div class="ide-shell-scaffold-loading" role="status" aria-label="Loading">
                      <Loader2 size={22} class="ide-shell-scaffold-loading__spin" />
                    </div>
                  }
                >
                  <Show
                    when={tab().kind === "session"}
                    fallback={
                      <FileEditor
                        projectId={projectId}
                        mainRevision={() => project.data?.main_revision ?? null}
                        tab={() => tab() as FileTab}
                        onPatch={(mutator) => patchFileTab(tab().id, mutator)}
                        onSaved={handleSaved}
                      />
                    }
                  >
                    <SessionTabView
                      projectId={projectId}
                      sessionId={() => (tab() as SessionTab).sessionId}
                      subView={() => (tab() as SessionTab).subView}
                      onSubViewChange={(view) =>
                        patchSessionTab(tab().id, (s) => {
                          s.subView = view;
                        })
                      }
                      onTitle={(title) =>
                        patchSessionTab(tab().id, (s) => {
                          s.title = title;
                        })
                      }
                    />
                  </Show>
                </Suspense>
              )}
            </Show>
          </div>
        </main>
      </div>
    </section>
  );
}

function TabView(props: {
  tab: MainTab;
  active: boolean;
  onActivate: () => void;
  onClose: () => void;
}) {
  const label = () =>
    props.tab.kind === "file" ? basename(props.tab.path) : props.tab.title || "Session";
  const title = () =>
    props.tab.kind === "file" ? props.tab.path : `Session ${props.tab.sessionId}`;
  const dirty = () =>
    props.tab.kind === "file" &&
    Boolean(props.tab.meta?.editable) &&
    props.tab.draft !== props.tab.saved;
  return (
    <div
      class="ide-tab"
      classList={{
        "ide-tab--active": props.active,
        "ide-tab--dirty": dirty(),
        "ide-tab--session": props.tab.kind === "session",
      }}
    >
      <button
        type="button"
        class="ide-tab-label"
        role="tab"
        aria-selected={props.active}
        aria-label={`${label()}${dirty() ? ", unsaved changes" : ""}${props.tab.kind === "session" ? " (Session)" : ""}`}
        title={title()}
        onClick={props.onActivate}
      >
        <Show when={props.tab.kind === "session"}>
          <MessageSquare size={12} class="ide-tab-kind-icon" />
        </Show>
        <Show when={dirty()}>
          <span class="ide-tab-dirty" aria-hidden="true" title="Unsaved changes" />
        </Show>
        <span>{label()}</span>
      </button>
      <button
        type="button"
        class="ide-tab-close"
        aria-label={`Close ${label()}`}
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
  disabled?: boolean;
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
      disabled={props.disabled}
      onClick={props.onClick}
    >
      {props.children}
      <span>{props.label}</span>
    </button>
  );
}
