import { createEffect, createSignal } from "solid-js";
import { useProject } from "../../lib/queries";
import { useIsMobile } from "../../lib/viewport";
import { WorkspaceActivityBar } from "./workspace/WorkspaceActivityBar";
import { WorkspaceDocuments } from "./workspace/WorkspaceDocuments";
import { WorkspaceHeader } from "./workspace/WorkspaceHeader";
import { WorkspaceSidebar } from "./workspace/WorkspaceSidebar";
import { createWorkspaceState } from "./workspace/workspaceState";
import "./workspace/workspace.css";

interface ProjectWorkspaceProps {
  projectId: string;
}

export function ProjectWorkspace(props: ProjectWorkspaceProps) {
  const projectId = () => props.projectId || undefined;
  const project = useProject(projectId);
  const compact = useIsMobile();
  const workspace = createWorkspaceState(compact);
  const [treeRefresh, setTreeRefresh] = createSignal(0);
  const [sessionCreating, setSessionCreating] = createSignal(false);
  let trackedProjectId: string | undefined;
  let lastMainRevision: string | null | undefined;

  createEffect(() => {
    const id = projectId();
    if (trackedProjectId !== undefined && trackedProjectId !== id) {
      workspace.reset();
      setTreeRefresh(0);
      setSessionCreating(false);
      lastMainRevision = undefined;
    }
    trackedProjectId = id;
  });

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
      setTreeRefresh((value) => value + 1);
    }
    lastMainRevision = revision;
  });

  const ready = () => project.data?.state === "ready";
  const branch = () => project.data?.current_branch ?? project.data?.repository.branch ?? null;

  return (
    <section class="project-page project-page--ide" aria-labelledby="project-title">
      <WorkspaceHeader
        project={project.data}
        loading={project.isLoading}
        error={project.error}
        onRetry={() => void project.refetch()}
      />

      <div
        class="ide-shell"
        classList={{
          "ide-shell--compact": compact(),
          "ide-shell--sidebar-collapsed": !workspace.navigationOpen(),
        }}
      >
        <WorkspaceActivityBar
          activity={workspace.activity()}
          navigationOpen={workspace.navigationOpen()}
          ready={ready()}
          onSelect={workspace.selectActivity}
        />
        <WorkspaceSidebar
          open={workspace.navigationOpen()}
          activity={workspace.activity()}
          projectId={projectId}
          ready={ready}
          branch={branch}
          activeFilePath={workspace.activeFilePath}
          activeSessionId={workspace.activeSessionId}
          sessionCreating={sessionCreating}
          onSessionCreating={(creating) => setSessionCreating(creating)}
          treeRefresh={treeRefresh}
          onOpenFile={workspace.openFile}
          onOpenSession={workspace.openSession}
          onSessionDeleted={workspace.closeSession}
        />
        <WorkspaceDocuments
          workspace={workspace}
          projectId={projectId}
          project={() => project.data}
          ready={ready}
          sessionCreating={sessionCreating}
          onFileSaved={() => {
            setTreeRefresh((value) => value + 1);
          }}
        />
      </div>
    </section>
  );
}
