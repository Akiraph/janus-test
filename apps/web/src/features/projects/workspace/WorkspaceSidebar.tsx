import { Match, Show, Switch } from "solid-js";
import { FileTreePanel } from "../../file-editor/FileTreePanel";
import { SessionsPanel } from "../../session/SessionsPanel";
import { ScmPanel } from "../../source-control/ScmPanel";
import { LazyTerminalPanel } from "../../terminal/LazyTerminalPanel";
import type { WorkspaceActivity } from "./workspaceState";

interface WorkspaceSidebarProps {
  open: boolean;
  activity: WorkspaceActivity;
  projectId: () => string | undefined;
  ready: () => boolean;
  branch: () => string | null;
  activeFilePath: () => string | null;
  activeSessionId: () => string | null;
  sessionCreating: () => boolean;
  onSessionCreating: (creating: boolean) => void;
  treeRefresh: () => number;
  onOpenFile: (path: string) => void;
  onOpenSession: (sessionId: string, title?: string | null) => void;
  onSessionDeleted: (sessionId: string) => void;
}

export function WorkspaceSidebar(props: WorkspaceSidebarProps) {
  return (
    <Show when={props.open}>
      <aside class="ide-sidebar" aria-label="Workspace sidebar">
        <Switch>
          <Match when={props.activity === "explorer"}>
            <FileTreePanel
              projectId={props.projectId}
              activePath={props.activeFilePath}
              onOpenFile={props.onOpenFile}
              refreshToken={props.treeRefresh}
            />
          </Match>
          <Match when={props.activity === "sessions"}>
            <SessionsPanel
              projectId={props.projectId}
              projectReady={props.ready}
              activeSessionId={props.activeSessionId}
              creating={props.sessionCreating}
              onCreatingChange={props.onSessionCreating}
              onOpenSession={props.onOpenSession}
              onSessionDeleted={props.onSessionDeleted}
            />
          </Match>
          <Match when={props.activity === "scm"}>
            <ScmPanel
              projectId={props.projectId}
              onOpenFile={props.onOpenFile}
              branch={props.branch}
            />
          </Match>
          <Match when={props.activity === "terminal"}>
            <div class="ide-sidebar-panel terminal-sidebar-panel">
              <LazyTerminalPanel
                projectId={props.projectId}
                title="Main Terminal"
                active={() => props.activity === "terminal"}
              />
            </div>
          </Match>
        </Switch>
      </aside>
    </Show>
  );
}
