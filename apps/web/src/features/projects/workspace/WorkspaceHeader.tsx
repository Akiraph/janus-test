import { A } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import GitBranch from "lucide-solid/icons/git-branch";
import { Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import type { ProjectView } from "../../../lib/api";

interface WorkspaceHeaderProps {
  project: ProjectView | undefined;
  loading: boolean;
  error: unknown;
  onRetry: () => void;
}

export function WorkspaceHeader(props: WorkspaceHeaderProps) {
  return (
    <header class="workspace-topbar">
      <A class="project-back" href="/">
        <ArrowLeft size={16} />
        Exit
      </A>
      <Show
        when={props.project}
        fallback={
          <Show
            when={!props.loading}
            fallback={
              <div class="workspace-title-row" aria-busy="true">
                <div class="workspace-name">
                  <span>Workspace:</span>
                  <h1 id="project-title">...</h1>
                </div>
              </div>
            }
          >
            <ErrorBlock
              message={props.error instanceof Error ? props.error.message : "Project not found"}
              retry={props.onRetry}
            />
          </Show>
        }
      >
        {(project) => (
          <div class="workspace-title-row">
            <div class="workspace-identity">
              <div class="workspace-name">
                <span>Workspace:</span>
                <h1 id="project-title">{project().name}</h1>
              </div>
              <p class="project-subtitle">
                {project().repository.url}
                <Show when={project().current_branch ?? project().repository.branch}>
                  {(branch) => (
                    <>
                      {" / "}
                      <GitBranch size={12} class="inline-icon" /> {branch()}
                    </>
                  )}
                </Show>
              </p>
            </div>
            <Badge
              variant={
                project().state === "ready"
                  ? "success"
                  : project().state === "error"
                    ? "danger"
                    : "warning"
              }
            >
              {project().state}
            </Badge>
          </div>
        )}
      </Show>
    </header>
  );
}
