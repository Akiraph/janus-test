import { Show } from "solid-js";
import { FileTreePanel } from "./FileTreePanel";
import { LazyTerminalPanel } from "./LazyTerminalPanel";
import { ScmPanel } from "./ScmPanel";
import { SessionsPanel } from "./SessionsPanel";
import type { WorkspaceActivity } from "./workspaceState";

interface WorkspaceSidebarProps {
  open: boolean;
  activity: WorkspaceActivity;
  projectId: () => string | undefined;
  ready: () => boolean;
  branch: () => string | null;
  activeFilePath: () => string | null;
  activeSessionId: () => string | null;
  treeRefresh: () => number;
  onOpenFile: (path: string) => void;
  onOpenSession: (sessionId: string, title?: string | null) => void;
  onSessionDeleted: (sessionId: string) => void;
}

export function WorkspaceSidebar(props: WorkspaceSidebarProps) {
  return (
    <Show when={props.open}>
      <aside class="ide-sidebar" aria-label="Workspace sidebar">
        <div class="ide-sidebar-view" hidden={props.activity !== "explorer"}>
          <FileTreePanel
            projectId={props.projectId}
            activePath={props.activeFilePath}
            onOpenFile={props.onOpenFile}
            refreshToken={props.treeRefresh}
          />
        </div>
        <div class="ide-sidebar-view" hidden={props.activity !== "sessions"}>
          <SessionsPanel
            projectId={props.projectId}
            projectReady={props.ready}
            activeSessionId={props.activeSessionId}
            onOpenSession={props.onOpenSession}
            onSessionDeleted={props.onSessionDeleted}
          />
        </div>
        <div class="ide-sidebar-view" hidden={props.activity !== "scm"}>
          <ScmPanel
            projectId={props.projectId}
            onOpenFile={props.onOpenFile}
            branch={props.branch}
          />
        </div>
        <div class="ide-sidebar-view" hidden={props.activity !== "terminal"}>
          <div class="ide-sidebar-panel terminal-sidebar-panel">
            <Show when={props.activity === "terminal"}>
              <LazyTerminalPanel
                projectId={props.projectId}
                title="Main Terminal"
                active={() => props.activity === "terminal"}
              />
            </Show>
          </div>
        </div>
      </aside>
    </Show>
  );
}
