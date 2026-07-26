import { A, useNavigate } from "@solidjs/router";
import { useQueryClient } from "@tanstack/solid-query";
import Folder from "lucide-solid/icons/folder";
import FolderGit2 from "lucide-solid/icons/folder-git-2";
import Plus from "lucide-solid/icons/plus";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Trash2 from "lucide-solid/icons/trash-2";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Badge, type BadgeVariant } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { EmptyState } from "../../components/ui/EmptyState";
import { ErrorBlock } from "../../components/ui/ErrorBlock";
import { useNotifications } from "../../components/ui/notifications";
import { Select, type SelectOption } from "../../components/ui/Select";
import type { CreateProjectInput, OperationView, ProjectView, RepoAccess } from "../../lib/api";
import {
  createProject,
  deleteProject,
  getOperation,
  getProject,
  retryProject,
} from "../../lib/api";
import { useGithubCredentials, useProjects } from "../../lib/queries";

const ACCESS_OPTIONS: readonly SelectOption[] = [
  { value: "public_https", label: "Public HTTPS" },
  { value: "github_private", label: "GitHub private (PAT)" },
];

function stateVariant(state: string): BadgeVariant {
  switch (state) {
    case "ready":
      return "success";
    case "creating":
    case "deleting":
      return "warning";
    case "error":
      return "danger";
    default:
      return "neutral";
  }
}

function formatActivity(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString();
}

function problemMessage(problem: unknown): string {
  if (!problem || typeof problem !== "object") return "Operation failed";
  const record = problem as { detail?: string; code?: string; title?: string };
  return record.detail ?? record.title ?? record.code ?? "Operation failed";
}

export function ProjectsPage() {
  const projects = useProjects();
  const queryClient = useQueryClient();
  const notify = useNotifications().notify;
  const navigate = useNavigate();
  const [formOpen, setFormOpen] = createSignal(false);
  const [tracking, setTracking] = createSignal<{
    operationId: string;
    projectId?: string;
    name: string;
  } | null>(null);
  const [progress, setProgress] = createSignal<OperationView | null>(null);

  async function refresh() {
    await queryClient.invalidateQueries({ queryKey: ["projects"] });
  }

  createEffect(() => {
    const current = tracking();
    if (!current) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const poll = async () => {
      try {
        const operation = await getOperation(current.operationId);
        if (cancelled) return;
        setProgress(operation);

        if (operation.status === "succeeded") {
          await refresh();
          let projectId = current.projectId ?? operation.target_id ?? undefined;
          if (!projectId) {
            const list = await queryClient.fetchQuery({
              queryKey: ["projects"],
              queryFn: () => import("../../lib/api").then((api) => api.listProjects()),
            });
            projectId = list.find((item) => item.name === current.name)?.id;
          }
          setTracking(null);
          setProgress(null);
          notify("Project ready", { variant: "success" });
          if (projectId) navigate(`/projects/${projectId}`);
          return;
        }

        if (
          operation.status === "failed" ||
          operation.status === "canceled" ||
          operation.status === "needs_attention"
        ) {
          await refresh();
          setTracking(null);
          setProgress(null);
          notify(problemMessage(operation.problem), { variant: "danger" });
          return;
        }

        // Still running: also refresh projects so state badges update.
        await refresh();
      } catch (error) {
        if (!cancelled) {
          notify(error instanceof Error ? error.message : "Could not poll operation", {
            variant: "danger",
          });
        }
      }
      if (!cancelled) timer = setTimeout(() => void poll(), 1500);
    };

    void poll();
    onCleanup(() => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    });
  });

  async function onRetry(project: ProjectView) {
    try {
      const operation = await retryProject(project.id);
      setTracking({
        operationId: operation.id,
        projectId: project.id,
        name: project.name,
      });
      setProgress(operation);
      notify("Retry started");
      await refresh();
    } catch (error) {
      notify(error instanceof Error ? error.message : "Retry failed", { variant: "danger" });
    }
  }

  async function onDelete(project: ProjectView) {
    if (!confirm(`Delete project “${project.name}”? This removes the local workspace.`)) return;
    try {
      const operation = await deleteProject(project.id, project.version, crypto.randomUUID());
      setTracking({
        operationId: operation.id,
        projectId: project.id,
        name: project.name,
      });
      setProgress(operation);
      notify("Delete started");
      await refresh();
    } catch (error) {
      notify(error instanceof Error ? error.message : "Delete failed", { variant: "danger" });
    }
  }

  return (
    <section class="projects route-enter" aria-labelledby="workspace-title">
      <div class="projects-heading projects-heading-row">
        <div>
          <h1 id="workspace-title">Projects</h1>
          <p>Pick a repository to start working.</p>
        </div>
        <Button variant="primary" onClick={() => setFormOpen(true)}>
          <Plus size={16} />
          Create project
        </Button>
      </div>

      <Show when={tracking()}>
        {(current) => (
          <article class="operation-card" aria-live="polite">
            <div class="record-title">
              <h3>{current().name}</h3>
              <Badge variant="warning">{progress()?.status ?? "queued"}</Badge>
            </div>
            <p>
              {progress()?.current_step
                ? `Step: ${progress()?.current_step}`
                : "Waiting for clone operation…"}
            </p>
            <p class="operation-meta">Operation {current().operationId}</p>
          </article>
        )}
      </Show>

      <Show
        when={!projects.isPending}
        fallback={
          <p class="files-tree-empty" role="status" aria-label="Loading...">
            Loading...
          </p>
        }
      >
        <Show
          when={!projects.isError}
          fallback={
            <ErrorBlock
              message={
                projects.error instanceof Error ? projects.error.message : "Failed to load projects"
              }
              retry={() => void projects.refetch()}
            />
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
                        <Badge variant={stateVariant(project.state)}>{project.state}</Badge>
                      </div>
                      <p class="project-repo">{project.repository.url}</p>
                      <div class="record-chips">
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
                          <FolderGit2 size={14} />
                          Open
                        </Button>
                      </Show>
                      <Show when={project.state === "error"}>
                        <Button variant="outline" size="sm" onClick={() => void onRetry(project)}>
                          <RefreshCw size={14} />
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
                          <Trash2 size={16} />
                        </Button>
                      </Show>
                    </div>
                  </article>
                )}
              </For>
            </div>
          </Show>
        </Show>
      </Show>

      <Show when={formOpen()}>
        <CreateProjectDialog
          close={() => setFormOpen(false)}
          created={async (operation, input) => {
            setFormOpen(false);
            // Best-effort: operation target_id may already be the project id.
            let projectId = operation.target_id ?? undefined;
            if (!projectId) {
              try {
                await refresh();
              } catch {
                /* ignore */
              }
            } else {
              try {
                await getProject(projectId);
              } catch {
                projectId = undefined;
              }
            }
            setTracking({
              operationId: operation.id,
              ...(projectId ? { projectId } : {}),
              name: input.name,
            });
            setProgress(operation);
            await refresh();
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
      const operation = await createProject(input, crypto.randomUUID());
      notify("Project creation started");
      await props.created(operation, input);
    } catch (value) {
      setError(value instanceof Error ? value.message : "Could not create project");
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
          <div class="full-field">
            <span class="field-label">Name</span>
            <input
              class="ui-input"
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              aria-label="Name"
              required
            />
          </div>
          <div class="full-field">
            <span class="field-label">Repository URL</span>
            <input
              class="ui-input"
              type="url"
              value={url()}
              onInput={(event) => setUrl(event.currentTarget.value)}
              placeholder="https://github.com/org/repo.git"
              aria-label="Repository URL"
              required
            />
          </div>
          <div>
            <span class="field-label">Branch (optional)</span>
            <input
              class="ui-input"
              value={branch()}
              onInput={(event) => setBranch(event.currentTarget.value)}
              placeholder="main"
              aria-label="Branch"
            />
          </div>
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
        <Show when={error()}>
          <ErrorBlock variant="inline" message={error()} />
        </Show>
        <div class="dialog-footer">
          <Button variant="outline" type="button" onClick={props.close}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={submitting()}>
            Create project
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
