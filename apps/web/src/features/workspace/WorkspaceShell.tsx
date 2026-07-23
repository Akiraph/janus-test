import {
  FileDiff,
  File as FileIcon,
  FileText,
  GitBranch,
  type LucideIcon,
  MessageSquare,
  TerminalSquare,
} from "lucide-react";
import { lazy, Suspense, useCallback, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/button";
import { Dialog } from "../../components/ui/dialog";
import { cn } from "../../lib/cn";
import { useProjects } from "../home/hooks/useProjects";
import { ActivityBar } from "./ActivityBar";
import { ConversationView } from "./conversation/ConversationView";
import { SessionDiffPanel } from "./conversation/SessionChangesSection";
import { ProjectSettingsDialog } from "./dialogs/ProjectSettingsDialog";
import { useProjectThreads } from "./hooks";
import { FilesPanel } from "./panels/FilesPanel";
import { GitPanel } from "./panels/GitPanel";
import { SessionsPanel } from "./panels/SessionsPanel";
import { TerminalsPanel } from "./panels/TerminalsPanel";
import { WorkspaceTabs } from "./WorkspaceTabs";
import { WorkspaceTopBar } from "./WorkspaceTopBar";

const FileViewer = lazy(() =>
  import("./panels/FileViewer").then((module) => ({
    default: module.FileViewer,
  })),
);
const TerminalPanel = lazy(() =>
  import("./panels/TerminalPanel").then((module) => ({
    default: module.TerminalPanel,
  })),
);

/** A side-bar provider — switches what the side bar lists, never an editor tab. */
type SidebarView = "sessions" | "files" | "git" | "terminal";

/** Far-left rail entries — every one switches what the side bar lists. */
const RAIL_ITEMS: ReadonlyArray<{
  readonly id: SidebarView;
  readonly label: string;
  readonly icon: LucideIcon;
}> = [
  { id: "sessions", label: "Sessions", icon: MessageSquare },
  { id: "files", label: "Files", icon: FileText },
  { id: "git", label: "Git", icon: GitBranch },
  { id: "terminal", label: "Terminal", icon: TerminalSquare },
];

/** An open document in the editor area — a concrete session, file, or terminal. */
interface DocTab {
  readonly key: string;
  readonly kind: "session" | "file" | "terminal" | "diff";
  readonly refId: string;
}

interface TerminalDoc {
  readonly id: string;
  readonly key: string;
  readonly label: string;
}

interface WorkspaceShellProps {
  projectId: string;
}

export function WorkspaceShell({ projectId }: WorkspaceShellProps) {
  const navigate = useNavigate();
  const { data: projectsData } = useProjects();
  const { data: threadsData } = useProjectThreads(projectId);

  // Side bar: which provider is shown (null = collapsed).
  const [sidebarView, setSidebarView] = useState<SidebarView | null>(
    "sessions",
  );
  // Editor area: open document tabs + which is focused.
  const [tabs, setTabs] = useState<DocTab[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [terminals, setTerminals] = useState<TerminalDoc[]>([]);
  const [terminalSeq, setTerminalSeq] = useState(0);
  const [projectSettingsOpen, setProjectSettingsOpen] = useState(false);
  // Per-file dirty flags, keyed by the file's DocTab key. Only file tabs can
  // be dirty; session/terminal tabs never appear here.
  const [dirtyMap, setDirtyMap] = useState<Record<string, boolean>>({});
  // When the user tries to close a dirty tab we pause to confirm: this holds
  // the key of the tab awaiting a Save / Don't-save / Cancel decision.
  const [pendingClose, setPendingClose] = useState<string | null>(null);
  const [savingClose, setSavingClose] = useState(false);
  const fileSaveActionsRef = useRef<Record<string, () => Promise<boolean>>>({});

  const project = projectsData?.projects.find((p) => p.id === projectId);
  const projectName = project ? `${project.owner}/${project.repo}` : "…";
  const threads = threadsData?.threads ?? [];

  const openDoc = useCallback((tab: DocTab) => {
    setTabs((prev) =>
      prev.some((t) => t.key === tab.key) ? prev : [...prev, tab],
    );
    setActiveKey(tab.key);
  }, []);

  const openSession = (sessionId: string) =>
    openDoc({ key: `session:${sessionId}`, kind: "session", refId: sessionId });
  const openFile = (path: string) =>
    openDoc({ key: `file:${path}`, kind: "file", refId: path });
  const openTerminal = useCallback(
    (terminalId: string) =>
      openDoc({
        key: `terminal:${terminalId}`,
        kind: "terminal",
        refId: terminalId,
      }),
    [openDoc],
  );
  const openSessionDiff = (sessionId: string) =>
    openDoc({ key: `diff:${sessionId}`, kind: "diff", refId: sessionId });
  const newTerminal = () => {
    const seq = terminalSeq + 1;
    const terminalId = String(seq);
    setTerminalSeq(seq);
    setTerminals((prev) => [
      ...prev,
      {
        id: terminalId,
        key: `terminal:${terminalId}`,
        label: `Terminal ${terminalId}`,
      },
    ]);
    openTerminal(terminalId);
  };

  const removeTab = useCallback((key: string) => {
    setTabs((prev) => {
      const idx = prev.findIndex((t) => t.key === key);
      const next = prev.filter((t) => t.key !== key);
      // Drop the dirty flag so it can't leak into a later tab reusing the key.
      setDirtyMap((prevDirty) => {
        if (!(key in prevDirty)) return prevDirty;
        const { [key]: _omit, ...rest } = prevDirty;
        return rest;
      });
      setActiveKey((current) => {
        if (current !== key) return current;
        const fallback = next[idx] ?? next[idx - 1] ?? next[0] ?? null;
        return fallback?.key ?? null;
      });
      return next;
    });
  }, []);

  // Closing a file tab: if it has unsaved edits, ask first. Other kinds close
  // immediately.
  const closeTab = (key: string) => {
    if (dirtyMap[key]) {
      setPendingClose(key);
      return;
    }
    removeTab(key);
  };

  const handleFileDirty = useCallback((path: string, dirty: boolean) => {
    const key = `file:${path}`;
    setDirtyMap((prev) =>
      prev[key] === dirty ? prev : { ...prev, [key]: dirty },
    );
  }, []);

  const registerFileSaveAction = useCallback(
    (path: string, action: (() => Promise<boolean>) | undefined) => {
      const key = `file:${path}`;
      if (action === undefined) {
        delete fileSaveActionsRef.current[key];
        return;
      }
      fileSaveActionsRef.current[key] = action;
    },
    [],
  );

  const savePendingClose = async () => {
    if (pendingClose === null) {
      return;
    }

    const saveAction = fileSaveActionsRef.current[pendingClose];
    if (saveAction === undefined) {
      removeTab(pendingClose);
      setPendingClose(null);
      return;
    }

    setSavingClose(true);
    const saved = await saveAction();
    setSavingClose(false);

    if (saved) {
      removeTab(pendingClose);
      setPendingClose(null);
    }
  };

  const selectSidebar = (id: SidebarView) =>
    setSidebarView((prev) => (prev === id ? null : id));

  const activeTab = tabs.find((t) => t.key === activeKey) ?? null;
  const activeSessionId =
    activeTab?.kind === "session" ? activeTab.refId : undefined;

  // Resolve a friendly label + icon for each open tab.
  const labelFor = (tab: DocTab): { label: string; icon: LucideIcon } => {
    if (tab.kind === "session") {
      const title = threads.find((t) => t.sessionId === tab.refId)?.title;
      return { label: title || "Session", icon: MessageSquare };
    }
    if (tab.kind === "file") {
      return { label: basename(tab.refId), icon: FileIcon };
    }
    if (tab.kind === "diff") {
      const title = threads.find((t) => t.sessionId === tab.refId)?.title;
      return {
        label: title ? `${title} diff` : "Session diff",
        icon: FileDiff,
      };
    }
    const terminal = terminals.find((item) => item.id === tab.refId);
    return {
      label: terminal?.label ?? `Terminal ${tab.refId}`,
      icon: TerminalSquare,
    };
  };

  const tabItems = tabs.map((tab) => {
    const { label, icon } = labelFor(tab);
    return { id: tab.key, label, icon, dirty: dirtyMap[tab.key] ?? false };
  });

  return (
    <div className="flex h-full w-full flex-col overflow-hidden rounded-lg border border-border bg-card shadow-sm">
      <WorkspaceTopBar
        projectName={projectName}
        onOpenWorkspaceSettings={() => setProjectSettingsOpen(true)}
        onOpenSettings={() => navigate("/settings")}
        onExit={() => navigate("/")}
      />

      <div className="flex min-h-0 flex-1 bg-background">
        <ActivityBar
          items={RAIL_ITEMS}
          activeId={sidebarView}
          onSelect={(id) => selectSidebar(id as SidebarView)}
        />

        {sidebarView && (
          <aside
            key={sidebarView}
            className="flex w-72 shrink-0 flex-col border-r border-border bg-background animate-panel-in"
          >
            {sidebarView === "sessions" && (
              <SessionsPanel
                projectId={projectId}
                selectedSessionId={activeSessionId}
                onSessionSelect={openSession}
                onSessionDeleted={(sessionId) =>
                  closeTab(`session:${sessionId}`)
                }
              />
            )}
            {sidebarView === "files" && (
              <FilesPanel projectId={projectId} onFileSelect={openFile} />
            )}
            {sidebarView === "git" && (
              <GitPanel projectId={projectId} onFileSelect={openFile} />
            )}
            {sidebarView === "terminal" && (
              <TerminalsPanel
                terminals={terminals}
                activeKey={activeKey}
                onSelect={(key) => {
                  const terminal = terminals.find((item) => item.key === key);

                  if (terminal !== undefined) {
                    openTerminal(terminal.id);
                  }
                }}
                onNew={newTerminal}
              />
            )}
          </aside>
        )}

        <div className="flex min-w-0 flex-1 flex-col">
          {tabs.length > 0 && (
            <WorkspaceTabs
              tabs={tabItems}
              activeId={activeKey}
              onSelect={setActiveKey}
              onClose={closeTab}
            />
          )}

          <div className="relative min-h-0 flex-1">
            {tabs.length === 0 ? (
              <EmptyEditor />
            ) : (
              tabs.map((tab) => (
                <div
                  key={tab.key}
                  className={cn(
                    "absolute inset-0",
                    tab.key === activeKey ? "" : "hidden",
                  )}
                >
                  {tab.kind === "session" && (
                    <ConversationView
                      sessionId={tab.refId}
                      projectId={projectId}
                      onOpenSession={openSession}
                      onOpenSessionDiff={openSessionDiff}
                    />
                  )}
                  {tab.kind === "file" && (
                    <Suspense fallback={<EditorLoading label="Loading file" />}>
                      <FileViewer
                        projectId={projectId}
                        path={tab.refId}
                        onDirtyChange={(dirty) =>
                          handleFileDirty(tab.refId, dirty)
                        }
                        onSaveActionChange={(action) =>
                          registerFileSaveAction(tab.refId, action)
                        }
                      />
                    </Suspense>
                  )}
                  {tab.kind === "terminal" && (
                    <Suspense
                      fallback={<EditorLoading label="Loading terminal" />}
                    >
                      <TerminalPanel
                        projectId={projectId}
                        terminalId={tab.refId}
                      />
                    </Suspense>
                  )}
                  {tab.kind === "diff" && (
                    <SessionDiffPanel sessionId={tab.refId} />
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      </div>

      <ProjectSettingsDialog
        projectId={projectId}
        open={projectSettingsOpen}
        onOpenChange={setProjectSettingsOpen}
      />

      <UnsavedCloseDialog
        tabKey={pendingClose}
        fileName={
          pendingClose?.startsWith("file:")
            ? basename(pendingClose.slice("file:".length))
            : ""
        }
        onCancel={() => setPendingClose(null)}
        onSaveAndClose={savePendingClose}
        saving={savingClose}
        onDiscard={() => {
          if (pendingClose !== null) removeTab(pendingClose);
          setPendingClose(null);
        }}
      />
    </div>
  );
}

/**
 * UnsavedCloseDialog — confirms before discarding unsaved edits when closing a
 * file tab. The Save action itself is done in-editor (Ctrl/Cmd+S); this dialog
 * only guards the discard path so users don't lose work by mis-clicking the ✕.
 */
function UnsavedCloseDialog({
  tabKey,
  fileName,
  onCancel,
  onSaveAndClose,
  saving,
  onDiscard,
}: {
  tabKey: string | null;
  fileName: string;
  onCancel: () => void;
  onSaveAndClose: () => void;
  saving: boolean;
  onDiscard: () => void;
}) {
  return (
    <Dialog
      open={tabKey !== null}
      onOpenChange={(open) => {
        if (!open) onCancel();
      }}
      title="Unsaved changes"
      description={
        fileName
          ? `"${fileName}" has unsaved changes. Save them before closing or discard them.`
          : "This tab has unsaved changes. Save them before closing or discard them."
      }
    >
      <div className="flex justify-end gap-2 pt-2">
        <Button variant="outline" onClick={onCancel} disabled={saving}>
          Cancel
        </Button>
        <Button onClick={onSaveAndClose} disabled={saving}>
          {saving ? "Saving..." : "Save and close"}
        </Button>
        <Button variant="destructive" onClick={onDiscard} disabled={saving}>
          Close without saving
        </Button>
      </div>
    </Dialog>
  );
}

function basename(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function EmptyEditor() {
  return (
    <div className="flex h-full flex-col items-center justify-center bg-background text-center text-muted-foreground">
      <MessageSquare className="h-12 w-12 text-muted-foreground/40" />
      <p className="mt-4 text-sm font-medium">Nothing open</p>
      <p className="mt-1 text-xs">
        Pick a session or a file from the sidebar to open it as a tab.
      </p>
    </div>
  );
}

function EditorLoading({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center bg-background text-sm text-muted-foreground">
      {label}...
    </div>
  );
}
