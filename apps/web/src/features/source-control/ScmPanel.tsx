import Check from "lucide-solid/icons/check";
import ChevronRight from "lucide-solid/icons/chevron-right";
import Loader2 from "lucide-solid/icons/loader-2";
import { createEffect, createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { NotificationEvent, useNotifications } from "../../components/ui/notifications";
import { SideScrollbar } from "../../components/ui/SideScrollbar";
import {
  type GitUpdateConflictView,
  getErrorMessage,
  gitCommit,
  gitFetch,
  gitPush,
  gitRemotes,
  gitStage,
  gitUnstage,
  gitUpdate,
  listGitUpdateConflicts,
  resolveGitUpdateConflict,
} from "../../lib/api";
import { useGitStatus } from "../../lib/queries";
import { ScmGraphList } from "./ScmGraphList";
import "./source-control.css";
import { basename } from "../../lib/utils";

interface ScmPanelProps {
  projectId: () => string | undefined;
  onOpenFile?: (path: string) => void;
  /* Branch hint for the inline graph header. */
  branch?: (() => string | null) | undefined;
}

const CONFLICT_CHOICES = [
  { value: "main", label: "Keep local" },
  { value: "remote", label: "Take remote" },
  { value: "delete", label: "Delete file" },
  { value: "edited_text", label: "Merge by hand" },
] as const;

export function ScmPanel(props: ScmPanelProps) {
  const notify = useNotifications().notify;
  const status = useGitStatus(props.projectId);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [message, setMessage] = createSignal("");
  const [pending, setPending] = createSignal("");
  const [confirming, setConfirming] = createSignal<"push" | "update" | null>(null);
  const [conflicts, setConflicts] = createSignal<GitUpdateConflictView[]>([]);
  const [choices, setChoices] = createSignal<Record<string, string>>({});
  const [editedText, setEditedText] = createSignal<Record<string, string>>({});
  const [stagedOpen, setStagedOpen] = createSignal(true);
  const [changesOpen, setChangesOpen] = createSignal(true);
  const [graphOpen, setGraphOpen] = createSignal(true);
  const [scrollHost, setScrollHost] = createSignal<HTMLElement | null>(null);
  const busy = () => pending() !== "";

  function toggle(path: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  async function refreshConflicts(id: string) {
    await listGitUpdateConflicts(id)
      .then(setConflicts)
      .catch(() => {
        // Conflicts endpoint failures shouldn't block status view.
      });
  }

  async function refreshGit() {
    const id = props.projectId();
    if (!id) return;
    // Status is fetched by the query on mount; refresh it explicitly only
    // after a user action. Conflict metadata remains independent.
    await Promise.all([status.refetch(), refreshConflicts(id)]);
  }

  createEffect(() => {
    const id = props.projectId();
    if (!id) return;
    void refreshConflicts(id);
  });

  async function run(
    action: () => Promise<void>,
    labels: { pending: string; success: string; failure: string },
  ) {
    setPending(labels.pending);
    try {
      await action();
      notify(labels.success, { variant: "success" });
      setSelected(new Set<string>());
      await refreshGit();
    } catch (error) {
      notify(`${labels.failure}: ${getErrorMessage(error, "the Git command reported an error")}`, {
        variant: "danger",
      });
    } finally {
      setPending("");
    }
  }

  async function resolveRemote(id: string): Promise<string> {
    const remotes = await gitRemotes(id);
    return remotes.includes("origin") ? "origin" : (remotes[0] ?? "origin");
  }

  async function runCommit() {
    const id = props.projectId();
    if (!id) return;
    await run(
      async () => {
        await gitCommit(id, message().trim());
        setMessage("");
      },
      { pending: "Committing…", success: "Committed", failure: "Commit failed" },
    );
  }

  async function runFetch() {
    const id = props.projectId();
    if (!id) return;
    await run(
      async () => {
        await gitFetch(id, await resolveRemote(id), crypto.randomUUID());
      },
      { pending: "Fetching…", success: "Fetch started", failure: "Fetch failed" },
    );
  }

  async function runUpdate() {
    const id = props.projectId();
    const branch = status.data?.branch;
    if (!id || !branch) return;
    await run(
      async () => {
        await gitUpdate(id, await resolveRemote(id), branch, crypto.randomUUID());
      },
      { pending: "Updating…", success: "Update started", failure: "Update failed" },
    );
  }

  async function runPush() {
    const id = props.projectId();
    const branch = status.data?.branch;
    if (!id || !branch) return;
    await run(
      async () => {
        await gitPush(id, await resolveRemote(id), branch, crypto.randomUUID());
      },
      { pending: "Pushing…", success: "Push started", failure: "Push failed" },
    );
  }

  const selectedPaths = () => [...selected()];
  const openConflict = () => conflicts()[0];
  const canCommit = () =>
    !busy() && message().trim().length > 0 && (status.data?.index.length ?? 0) > 0;
  const conflictReady = () => {
    const conflict = openConflict();
    if (!conflict) return false;
    return conflict.paths.every((path) => Boolean(choices()[path.path]));
  };
  const pushDescription = () => {
    const ahead = status.data?.ahead ?? 0;
    const branch = status.data?.branch ?? "the current branch";
    const commits = ahead === 1 ? "1 local commit" : `${ahead} local commits`;
    return `Publishes ${commits} from ${branch} to the project's remote. Anyone with access to the remote sees them, and Janus cannot undo a push.`;
  };
  const updateDescription = () => {
    const branch = status.data?.branch ?? "the current branch";
    return `Fetches the remote and fast-forwards ${branch}. If the branches have diverged, Janus records an update conflict in this panel instead of changing your files.`;
  };
  const changeRows = () => {
    const data = status.data;
    if (!data) return [] as { path: string; letter: string }[];
    const working = data.working.map((path) => ({ path, letter: "M" }));
    const untracked = data.untracked.map((path) => ({ path, letter: "U" }));
    return [...working, ...untracked].sort((a, b) => a.path.localeCompare(b.path));
  };

  return (
    <section class="ide-sidebar-panel scm-panel" aria-label="Source Control">
      <NotificationEvent
        message={status.isError ? getErrorMessage(status.error, "Git status failed") : null}
        variant="danger"
        action={{ label: "Retry", onClick: () => void status.refetch() }}
      />
      <div class="ide-sidebar-header">Source Control</div>
      <Show
        when={status.data}
        fallback={
          <Show when={!status.isError}>
            <p class="surface-note" role="status">
              Loading…
            </p>
          </Show>
        }
      >
        {(data) => (
          <div class="ide-scroll-host">
            <div class="scm-body ide-sidebar-scroll" ref={setScrollHost}>
              <div class="scm-summary">
                <span class="scm-ahead-behind">
                  <span aria-hidden="true">
                    ↑{data().ahead} ↓{data().behind}
                  </span>
                  <span class="sr-only">
                    {data().ahead} commits ahead of the remote, {data().behind} behind
                  </span>
                </span>
                <Show when={pending()}>
                  <span class="scm-pending" role="status">
                    <Loader2 size={12} class="ui-spinner" aria-hidden="true" />
                    {pending()}
                  </span>
                </Show>
              </div>

              <div class="scm-commit">
                <textarea
                  class="ui-input scm-commit-message"
                  value={message()}
                  onInput={(event) => setMessage(event.currentTarget.value)}
                  placeholder="Message (Ctrl+Enter to commit)"
                  aria-label="Commit message"
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                      event.preventDefault();
                      if (canCommit()) void runCommit();
                    }
                  }}
                />
                <Button
                  variant="primary"
                  size="sm"
                  disabled={!canCommit()}
                  title={commitHint(data().index.length, message().trim().length > 0)}
                  onClick={() => void runCommit()}
                >
                  Commit
                </Button>
              </div>

              <div class="scm-actions">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || selectedPaths().length === 0}
                  title={stageHint(selectedPaths().length)}
                  onClick={() =>
                    void run(() => gitStage(props.projectId() as string, selectedPaths()), {
                      pending: "Staging…",
                      success: "Staged",
                      failure: "Stage failed",
                    })
                  }
                >
                  Stage
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || selectedPaths().length === 0}
                  title={unstageHint(selectedPaths().length)}
                  onClick={() =>
                    void run(() => gitUnstage(props.projectId() as string, selectedPaths()), {
                      pending: "Unstaging…",
                      success: "Unstaged",
                      failure: "Unstage failed",
                    })
                  }
                >
                  Unstage
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy()}
                  title="Fetch remote refs — your files stay untouched"
                  onClick={() => void runFetch()}
                >
                  Fetch
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || !data().branch}
                  title={updateHint(data().branch)}
                  onClick={() => setConfirming("update")}
                >
                  Update
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || !data().branch}
                  title={pushHint(data().branch)}
                  onClick={() => setConfirming("push")}
                >
                  Push
                </Button>
              </div>

              <ScmSection
                title="Staged Changes"
                count={data().index.length}
                open={stagedOpen()}
                onToggle={() => setStagedOpen((v) => !v)}
              >
                <Show
                  when={data().index.length > 0}
                  fallback={<p class="surface-note">No staged changes</p>}
                >
                  <ul class="scm-path-list">
                    <For each={data().index}>
                      {(path) => (
                        <ScmPathRow
                          path={path}
                          letter="M"
                          selected={selected().has(path)}
                          onToggle={() => toggle(path)}
                          onOpen={() => props.onOpenFile?.(path)}
                        />
                      )}
                    </For>
                  </ul>
                </Show>
              </ScmSection>

              <ScmSection
                title="Changes"
                count={changeRows().length}
                open={changesOpen()}
                onToggle={() => setChangesOpen((v) => !v)}
              >
                <Show
                  when={changeRows().length > 0}
                  fallback={<p class="surface-note">No changes</p>}
                >
                  <ul class="scm-path-list">
                    <For each={changeRows()}>
                      {(row) => (
                        <ScmPathRow
                          path={row.path}
                          letter={row.letter}
                          selected={selected().has(row.path)}
                          onToggle={() => toggle(row.path)}
                          onOpen={() => props.onOpenFile?.(row.path)}
                        />
                      )}
                    </For>
                  </ul>
                </Show>
              </ScmSection>

              <Show when={openConflict()}>
                {(conflict) => (
                  <section class="git-conflict" aria-label="Git update conflict">
                    <h2>Update conflict</h2>
                    <p>
                      Remote changes collide with local working-tree edits. Choose a side for each
                      path, then apply.
                    </p>
                    <p>Applying rewrites these paths in your working tree.</p>
                    <For each={conflict().paths}>
                      {(path) => (
                        <div class="git-conflict-path">
                          <strong>{path.path}</strong>
                          <span class="scm-path-meta">{conflictKindLabel(path.kind)}</span>
                          <div class="git-actions">
                            <For each={CONFLICT_CHOICES}>
                              {(choice) => (
                                <Button
                                  variant={
                                    choices()[path.path] === choice.value ? "primary" : "outline"
                                  }
                                  size="sm"
                                  aria-label={choiceLabel(
                                    choice.label,
                                    path.path,
                                    choices()[path.path] === choice.value,
                                  )}
                                  onClick={() =>
                                    setChoices((current) => ({
                                      ...current,
                                      [path.path]: choice.value,
                                    }))
                                  }
                                >
                                  <Show when={choices()[path.path] === choice.value}>
                                    <Check size={12} aria-hidden="true" />
                                  </Show>
                                  {choice.label}
                                </Button>
                              )}
                            </For>
                          </div>
                          <Show when={choices()[path.path] === "edited_text"}>
                            <textarea
                              class="ui-input git-commit-message"
                              value={editedText()[path.path] ?? ""}
                              onInput={(event) =>
                                setEditedText((current) => ({
                                  ...current,
                                  [path.path]: event.currentTarget.value,
                                }))
                              }
                              placeholder="Merged file content"
                              aria-label={`Edited text for ${path.path}`}
                            />
                          </Show>
                        </div>
                      )}
                    </For>
                    <Button
                      variant="primary"
                      size="sm"
                      disabled={busy() || !conflictReady()}
                      title={
                        conflictReady()
                          ? "Rewrite these files with the chosen sides"
                          : "Choose a side for every path first"
                      }
                      onClick={() =>
                        void run(
                          async () => {
                            const id = props.projectId() as string;
                            const current = openConflict();
                            if (!current) return;
                            const paths = current.paths.map((path) => {
                              const choice = choices()[path.path] ?? "main";
                              return choice === "edited_text"
                                ? {
                                    path: path.path,
                                    choice,
                                    edited_text: editedText()[path.path] ?? "",
                                  }
                                : {
                                    path: path.path,
                                    choice,
                                  };
                            });
                            await resolveGitUpdateConflict(id, current.id, current.version, {
                              paths,
                            });
                            setChoices({});
                            setEditedText({});
                          },
                          {
                            pending: "Applying resolution…",
                            success: "Conflict resolved",
                            failure: "Resolution failed",
                          },
                        )
                      }
                    >
                      Apply resolution
                    </Button>
                  </section>
                )}
              </Show>

              <section class="scm-section scm-graph-section">
                <button
                  type="button"
                  class="scm-section-header"
                  aria-expanded={graphOpen()}
                  onClick={() => setGraphOpen((v) => !v)}
                >
                  <span
                    class="scm-section-chevron"
                    classList={{ "scm-section-chevron--open": graphOpen() }}
                    aria-hidden="true"
                  >
                    <ChevronRight size={14} />
                  </span>
                  <span>Graph</span>
                </button>
                <div
                  class="scm-section-body scm-section-body--graph"
                  classList={{ "scm-section-body--open": graphOpen() }}
                  aria-hidden={!graphOpen()}
                >
                  <div class="scm-section-body-inner">
                    <ScmGraphList projectId={props.projectId} branch={props.branch} />
                  </div>
                </div>
              </section>
            </div>
            <SideScrollbar host={scrollHost} />
          </div>
        )}
      </Show>

      <Show when={confirming()}>
        {(kind) => (
          <Dialog
            title={kind() === "push" ? "Push to the remote?" : "Update from the remote?"}
            description={kind() === "push" ? pushDescription() : updateDescription()}
            close={() => setConfirming(null)}
          >
            <div class="dialog-footer">
              <Button variant="outline" size="sm" onClick={() => setConfirming(null)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={busy()}
                onClick={() => {
                  const action = kind();
                  setConfirming(null);
                  if (action === "push") void runPush();
                  else void runUpdate();
                }}
              >
                {kind() === "push" ? "Push" : "Update"}
              </Button>
            </div>
          </Dialog>
        )}
      </Show>
    </section>
  );
}

function ScmSection(props: {
  title: string;
  count: number;
  open: boolean;
  onToggle: () => void;
  children: import("solid-js").JSX.Element;
}) {
  return (
    <section class="scm-section">
      <button
        type="button"
        class="scm-section-header"
        aria-expanded={props.open}
        onClick={props.onToggle}
      >
        <span
          class="scm-section-chevron"
          classList={{ "scm-section-chevron--open": props.open }}
          aria-hidden="true"
        >
          <ChevronRight size={14} />
        </span>
        <span>
          {props.title} <span class="scm-path-meta">({props.count})</span>
        </span>
      </button>
      <div
        class="scm-section-body"
        classList={{ "scm-section-body--open": props.open }}
        aria-hidden={!props.open}
      >
        <div class="scm-section-body-inner">{props.children}</div>
      </div>
    </section>
  );
}

function ScmPathRow(props: {
  path: string;
  letter: string;
  selected: boolean;
  onToggle: () => void;
  onOpen: () => void;
}) {
  const letterLabel = () => (props.letter === "U" ? "Untracked" : "Modified");

  return (
    <li class="scm-path-item">
      <input
        type="checkbox"
        checked={props.selected}
        aria-label={`Select ${props.path}`}
        onChange={props.onToggle}
      />
      <button
        type="button"
        class="scm-path-open"
        aria-label={`Open ${props.path}, ${letterLabel()}`}
        title={`Open ${props.path}`}
        onClick={props.onOpen}
      >
        <span class="scm-path-name">
          <span>{basename(props.path)}</span>
          <Show when={props.path.includes("/")}>
            <span class="scm-path-dir">{props.path}</span>
          </Show>
        </span>
      </button>
      <span
        classList={{
          "scm-letter": true,
          "scm-letter--m": props.letter === "M",
          "scm-letter--u": props.letter === "U",
        }}
        title={letterLabel()}
        aria-hidden="true"
      >
        {props.letter}
      </span>
    </li>
  );
}

function commitHint(staged: number, hasMessage: boolean): string {
  if (staged === 0) return "Stage changes before committing";
  if (!hasMessage) return "Write a commit message first";
  return "Commit staged changes (Ctrl+Enter)";
}

function stageHint(count: number): string {
  return count === 0 ? "Tick files below to stage them" : `Stage ${count} selected path(s)`;
}

function unstageHint(count: number): string {
  return count === 0 ? "Tick files below to unstage them" : `Unstage ${count} selected path(s)`;
}

function updateHint(branch: string | null | undefined): string {
  return branch ? "Fetch and fast-forward this branch" : "No branch is checked out";
}

function pushHint(branch: string | null | undefined): string {
  return branch ? "Publish local commits to the remote" : "No branch is checked out";
}

function choiceLabel(label: string, path: string, selected: boolean): string {
  return selected ? `${label} for ${path}, selected` : `${label} for ${path}`;
}

/** Conflict kinds arrive as backend enum strings such as `both_modified`. */
function conflictKindLabel(kind: string): string {
  return kind.replaceAll("_", " ");
}
