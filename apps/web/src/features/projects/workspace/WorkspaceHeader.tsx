import { A } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import GitBranch from "lucide-solid/icons/git-branch";
import { Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { NotificationEvent } from "../../../components/ui/notifications";
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
      <NotificationEvent
        message={
          props.error
            ? props.error instanceof Error
              ? props.error.message
              : "Project not found"
            : null
        }
        variant="danger"
        action={{ label: "Retry", onClick: props.onRetry }}
      />
      <A class="project-back" href="/">
        <ArrowLeft size={16} />
        Exit
      </A>
      <Show
        when={props.project}
        fallback={
          <div class="workspace-title-row" aria-busy={props.loading}>
            <div class="workspace-name">
              <span>Workspace:</span>
              <h1 id="project-title">{props.loading ? "..." : "Unavailable"}</h1>
            </div>
          </div>
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
