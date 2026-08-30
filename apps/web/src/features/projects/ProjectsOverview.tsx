import { A, useNavigate } from "@solidjs/router";
import { useQueryClient } from "@tanstack/solid-query";
import Folder from "lucide-solid/icons/folder";
import FolderGit2 from "lucide-solid/icons/folder-git-2";
import Loader2 from "lucide-solid/icons/loader-2";
import Plus from "lucide-solid/icons/plus";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Trash2 from "lucide-solid/icons/trash-2";
import { createEffect, createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { EmptyState } from "../../components/ui/EmptyState";
import { NotificationEvent, useNotifications } from "../../components/ui/notifications";
import { Select, type SelectOption } from "../../components/ui/Select";
import type { CreateProjectInput, OperationView, ProjectView, RepoAccess } from "../../lib/api";
import { createProject, deleteProject, getErrorMessage, randomUuid, retryProject } from "../../lib/api";
import { useGithubCredentials, useOperation, useProjects } from "../../lib/queries";
import "./projects.css";

const ACCESS_OPTIONS: readonly SelectOption[] = [
  { value: "public_https", label: "Public HTTPS" },
  { value: "github_private", label: "GitHub private (PAT)" },
];

function formatActivity(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString();
}

function problemMessage(problem: unknown): string {
  return getErrorMessage(problem, "Operation failed");
}

function stateLabel(state: string): string {
  switch (state) {
    case "creating":
      return "Cloning repository…";
    case "error":
      return "Clone failed";
    case "deleting":
      return "Deleting…";
    default:
      return state;
  }
}

export function ProjectsOverview() {
  const projects = useProjects();
  const queryClient = useQueryClient();
  const notify = useNotifications().notify;
  const navigate = useNavigate();
  const [formOpen, setFormOpen] = createSignal(false);
  const [tracking, setTracking] = createSignal<{
    operationId: string;
    projectId?: string;
    kind: "create" | "retry" | "delete";
  } | null>(null);
  const trackedOperation = useOperation(() => tracking()?.operationId);
  let handledOperationId: string | null = null;

  createEffect(() => {
    const current = tracking();
    const operation = trackedOperation.data;
    if (
      !current ||
      !operation ||
      operation.id !== current.operationId ||
      handledOperationId === operation.id
    ) {
      return;
    }
    if (operation.status === "succeeded") {
      handledOperationId = operation.id;
      setTracking(null);
      if (current.kind === "delete") {
        notify("Project deleted", { variant: "success" });
        return;
      }
      notify("Project ready", { variant: "success" });
      const projectId = current.projectId ?? operation.target_id ?? undefined;
      if (current.kind === "retry" && projectId) navigate(`/projects/${projectId}`);
      return;
    }
    if (
      operation.status === "failed" ||
      operation.status === "canceled" ||
      operation.status === "needs_attention"
    ) {
      handledOperationId = operation.id;
      setTracking(null);
      notify(problemMessage(operation.problem), { variant: "danger" });
    }
  });

  async function onRetry(project: ProjectView) {
    try {
      const operation = await retryProject(project.id);
      setTracking({
        operationId: operation.id,
        projectId: project.id,
        kind: "retry",
      });
      notify("Retry started");
      queryClient.setQueryData(["operations", operation.id], operation);
    } catch (error) {
      notify(getErrorMessage(error, "Retry failed"), { variant: "danger" });
    }
  }

  async function onDelete(project: ProjectView) {
    if (!confirm(`Delete project “${project.name}”? This removes the local workspace.`)) return;
    try {
      const operation = await deleteProject(project.id, project.version, randomUuid());
      setTracking({
        operationId: operation.id,
        projectId: project.id,
        kind: "delete",
      });
      notify("Delete started");
      queryClient.setQueryData(["operations", operation.id], operation);
    } catch (error) {
      notify(getErrorMessage(error, "Delete failed"), { variant: "danger" });
    }
  }

  return (
    <section class="projects" aria-labelledby="projects-title">
      <NotificationEvent
        message={
          projects.isError ? getErrorMessage(projects.error, "Failed to load projects") : null
        }
        variant="danger"
        action={{ label: "Retry", onClick: () => void projects.refetch() }}
      />
      <div class="projects-heading projects-heading-row">
        <div>
          <h1 id="projects-title">Projects</h1>
          <p>Pick a repository to start working.</p>
        </div>
        <Button variant="primary" onClick={() => setFormOpen(true)}>
          <Plus size={16} aria-hidden="true" />
          Create project
        </Button>
      </div>

      <Show
        when={!projects.isPending}
        fallback={
          <p class="surface-note" role="status" aria-label="Loading...">
            Loading...
          </p>
        }
      >
        <Show
          when={(projects.data?.length ?? 0) > 0}
          fallback={
            <EmptyState
              icon={Folder}
              title="No projects yet"
              description="Create a project from a public or private Git repository."
              action={
                <Button variant="primary" onClick={() => setFormOpen(true)}>
                  Create project
                </Button>
              }
            />
          }
        >
          <div class="record-list">
            <For each={projects.data}>
              {(project) => (
                <article class="record-card project-card">
                  <div class="record-copy">
                    <div class="record-title">
                      <h3>
                        <Show when={project.state === "ready"} fallback={project.name}>
                          <A href={`/projects/${project.id}`}>{project.name}</A>
                        </Show>
                      </h3>
                    </div>
                    <p class="project-repo">{project.repository.url}</p>
                    <div class="record-chips">
                      <Show when={project.state !== "ready"}>
                        <span class="project-state" data-state={project.state}>
                          <Show when={project.state !== "error"}>
                            <Loader2 size={12} class="ui-spinner" aria-hidden="true" />
                          </Show>
                          {stateLabel(project.state)}
                        </span>
                      </Show>
                      <span>{project.current_branch ?? project.repository.branch ?? "—"}</span>
                      <span>{project.repository.access}</span>
                      <span>Updated {formatActivity(project.updated_at)}</span>
                    </div>
                  </div>
                  <div class="record-actions">
                    <Show when={project.state === "ready"}>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => navigate(`/projects/${project.id}`)}
                      >
                        <FolderGit2 size={14} aria-hidden="true" />
                        Open
                      </Button>
                    </Show>
                    <Show when={project.state === "error"}>
                      <Button variant="outline" size="sm" onClick={() => void onRetry(project)}>
                        <RefreshCw size={14} aria-hidden="true" />
                        Retry
                      </Button>
                    </Show>
                    <Show when={project.state === "error" || project.state === "ready"}>
                      <Button
                        variant="ghost"
                        size="sm"
                        iconOnly
                        aria-label={`Delete ${project.name}`}
                        onClick={() => void onDelete(project)}
                      >
                        <Trash2 size={16} aria-hidden="true" />
                      </Button>
                    </Show>
                  </div>
                </article>
              )}
            </For>
          </div>
        </Show>
      </Show>

      <Show when={formOpen()}>
        <CreateProjectDialog
          close={() => setFormOpen(false)}
          created={async (operation) => {
            setFormOpen(false);
            const projectId = operation.target_id ?? undefined;
            queryClient.setQueryData(["operations", operation.id], operation);
            setTracking({
              operationId: operation.id,
              ...(projectId ? { projectId } : {}),
              kind: "create",
            });
          }}
        />
      </Show>
    </section>
  );
}

interface CreateProjectDialogProps {
  close: () => void;
  created: (operation: OperationView, input: CreateProjectInput) => Promise<void>;
}

function CreateProjectDialog(props: CreateProjectDialogProps) {
  const notify = useNotifications().notify;
  const credentials = useGithubCredentials();
  const [name, setName] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [branch, setBranch] = createSignal("");
  const [access, setAccess] = createSignal<RepoAccess>("public_https");
  const [credentialId, setCredentialId] = createSignal("");
  const [error, setError] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);

  const credentialOptions = (): SelectOption[] => {
    const list = credentials.data ?? [];
    if (list.length === 0) return [{ value: "", label: "No credentials configured" }];
    return list.map((item) => ({
      value: item.id,
      label: `${item.name} (${item.github_host})`,
    }));
  };

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!name().trim()) {
      setError("Name is required");
      return;
    }
    if (!url().trim()) {
      setError("Repository URL is required");
      return;
    }
    if (access() === "github_private" && !credentialId()) {
      setError("Select a GitHub credential for private repositories");
      return;
    }

    const input: CreateProjectInput = {
      name: name().trim(),
      repository: {
        access: access(),
        url: url().trim(),
        branch: branch().trim() || null,
        github_credential_id: access() === "github_private" ? credentialId() || null : null,
      },
    };

    setSubmitting(true);
    setError("");
    try {
      const operation = await createProject(input, randomUuid());
      notify("Project creation started");
      await props.created(operation, input);
    } catch (value) {
      setError(getErrorMessage(value, "Could not create project"));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog
      title="Create project"
      description="Clone a Git repository into a Main Workspace."
      close={props.close}
    >
      <form class="dialog-form" onSubmit={submit}>
        <div class="dialog-form-grid">
          <label class="full-field">
            <span class="field-label">Name</span>
            <input
              class="ui-input"
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              required
            />
          </label>
          <label class="full-field">
            <span class="field-label">Repository URL</span>
            <input
              class="ui-input"
              type="url"
              value={url()}
              onInput={(event) => setUrl(event.currentTarget.value)}
              placeholder="https://github.com/org/repo.git"
              required
            />
          </label>
          <label>
            <span class="field-label">Branch (optional)</span>
            <input
              class="ui-input"
              value={branch()}
              onInput={(event) => setBranch(event.currentTarget.value)}
              placeholder="main"
            />
          </label>
          <div>
            <span class="field-label">Access</span>
            <Select
              value={access()}
              options={ACCESS_OPTIONS}
              onChange={(value) => setAccess(value as RepoAccess)}
              aria-label="Access"
            />
          </div>
          <Show when={access() === "github_private"}>
            <div class="full-field">
              <span class="field-label">GitHub credential</span>
              <Select
                value={credentialId()}
                options={credentialOptions()}
                onChange={setCredentialId}
                aria-label="GitHub credential"
              />
            </div>
          </Show>
        </div>
        <NotificationEvent message={error()} variant="danger" />
        <div class="dialog-footer">
          <Button variant="outline" type="button" onClick={props.close}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={submitting()}>
            {submitting() ? "Creating project…" : "Create project"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
