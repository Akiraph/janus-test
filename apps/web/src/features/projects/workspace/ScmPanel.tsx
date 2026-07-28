import { useQueryClient } from "@tanstack/solid-query";
import ChevronRight from "lucide-solid/icons/chevron-right";
import { createEffect, createSignal, For, Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { Button } from "../../../components/ui/Button";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import { useNotifications } from "../../../components/ui/notifications";
import { SideScrollbar } from "../../../components/ui/SideScrollbar";
import type { GitUpdateConflictView } from "../../../lib/api";
import {
  gitCommit,
  gitFetch,
  gitPush,
  gitRemotes,
  gitStage,
  gitUnstage,
  gitUpdate,
  listGitUpdateConflicts,
  resolveGitUpdateConflict,
} from "../../../lib/api";
import { useGitStatus } from "../../../lib/queries";
import { ScmGraphList } from "./ScmGraphList";
import { basename } from "./utils";

interface ScmPanelProps {
  projectId: () => string | undefined;
  onOpenFile?: (path: string) => void;
  /* Branch hint for the inline graph header. */
  branch?: (() => string | null) | undefined;
}

export function ScmPanel(props: ScmPanelProps) {
  const notify = useNotifications().notify;
  const queryClient = useQueryClient();
  const status = useGitStatus(props.projectId);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [conflicts, setConflicts] = createSignal<GitUpdateConflictView[]>([]);
  const [choices, setChoices] = createSignal<Record<string, string>>({});
  const [editedText, setEditedText] = createSignal<Record<string, string>>({});
  const [stagedOpen, setStagedOpen] = createSignal(true);
  const [changesOpen, setChangesOpen] = createSignal(true);
  const [graphOpen, setGraphOpen] = createSignal(true);
  const [scrollHost, setScrollHost] = createSignal<HTMLElement | null>(null);

  function toggle(path: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  async function refreshGit() {
    const id = props.projectId();
    if (!id) return;
    // Fire status (re-fetch) and conflict checks in parallel — they have no
    // dependency on each other, so awaiting one before the other just stalls
    // the SCM panel behind git-status when conflicts could resolve first.
    const conflictsP = listGitUpdateConflicts(id)
      .then(setConflicts)
      .catch(() => {
        // Conflicts endpoint failures shouldn't block status view.
      });
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["git-status", id] }),
      conflictsP,
    ]);
  }

  createEffect(() => {
    const id = props.projectId();
    if (!id) return;
    void refreshGit();
  });

  async function run(action: () => Promise<void>, success: string) {
    setBusy(true);
    try {
      await action();
      notify(success, { variant: "success" });
      setSelected(new Set<string>());
      await refreshGit();
    } catch (error) {
      notify(error instanceof Error ? error.message : "Git command failed", { variant: "danger" });
    } finally {
      setBusy(false);
    }
  }

  const selectedPaths = () => [...selected()];
  const openConflict = () => conflicts()[0];
  const changeRows = () => {
    const data = status.data;
    if (!data) return [] as { path: string; letter: string }[];
    const working = data.working.map((path) => ({ path, letter: "M" }));
    const untracked = data.untracked.map((path) => ({ path, letter: "U" }));
    return [...working, ...untracked].sort((a, b) => a.path.localeCompare(b.path));
  };

  return (
    <section class="ide-sidebar-panel scm-panel" aria-label="Source Control">
      <div class="ide-sidebar-header">Source Control</div>
      <Show
        when={status.data}
        fallback={
          <Show when={status.isError} fallback={<p class="files-tree-empty">Loading...</p>}>
            <ErrorBlock
              message={status.error instanceof Error ? status.error.message : "Git status failed"}
              retry={() => void status.refetch()}
            />
          </Show>
        }
      >
        {(data) => (
          <div class="ide-scroll-host">
            <div class="scm-body ide-sidebar-scroll" ref={setScrollHost}>
              <div class="scm-summary">
                <Badge variant="neutral">
                  {data().branch ?? "detached"}
                  {data().head_sha ? ` · ${data().head_sha?.slice(0, 7)}` : ""}
                </Badge>
                <span class="scm-ahead-behind">
                  ↑{data().ahead} ↓{data().behind}
                </span>
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
                      if (!busy() && message().trim() && data().index.length > 0) {
                        void run(async () => {
                          await gitCommit(props.projectId() as string, message().trim());
                          setMessage("");
                        }, "Committed");
                      }
                    }
                  }}
                />
                <Button
                  variant="primary"
                  size="sm"
                  disabled={busy() || !message().trim() || data().index.length === 0}
                  onClick={() =>
                    void run(async () => {
                      await gitCommit(props.projectId() as string, message().trim());
                      setMessage("");
                    }, "Committed")
                  }
                >
                  Commit
                </Button>
              </div>

              <div class="scm-actions">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || selectedPaths().length === 0}
                  onClick={() =>
                    void run(() => gitStage(props.projectId() as string, selectedPaths()), "Staged")
                  }
                >
                  Stage
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || selectedPaths().length === 0}
                  onClick={() =>
                    void run(
                      () => gitUnstage(props.projectId() as string, selectedPaths()),
                      "Unstaged",
                    )
                  }
                >
                  Unstage
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy()}
                  onClick={() =>
                    void run(async () => {
                      const id = props.projectId() as string;
                      const remotes = await gitRemotes(id);
                      const remote = remotes.includes("origin")
                        ? "origin"
                        : (remotes[0] ?? "origin");
                      await gitFetch(id, remote, crypto.randomUUID());
                    }, "Fetch started")
                  }
                >
                  Fetch
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || !data().branch}
                  onClick={() =>
                    void run(async () => {
                      const id = props.projectId() as string;
                      const remotes = await gitRemotes(id);
                      const remote = remotes.includes("origin")
                        ? "origin"
                        : (remotes[0] ?? "origin");
                      const branch = data().branch ?? "main";
                      await gitUpdate(id, remote, branch, crypto.randomUUID());
                    }, "Update started")
                  }
                >
                  Update
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy() || !data().branch}
                  onClick={() =>
                    void run(async () => {
                      const id = props.projectId() as string;
                      const remotes = await gitRemotes(id);
                      const remote = remotes.includes("origin")
                        ? "origin"
                        : (remotes[0] ?? "origin");
                      const branch = data().branch ?? "main";
                      await gitPush(id, remote, branch, crypto.randomUUID());
                    }, "Push started")
                  }
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
                  fallback={<p class="files-tree-empty">No staged changes</p>}
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
                  fallback={<p class="files-tree-empty">No changes</p>}
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
                    <For each={conflict().paths}>
                      {(path) => (
                        <div class="git-conflict-path">
                          <strong>{path.path}</strong>
                          <span class="files-editor-meta">{path.kind}</span>
                          <div class="git-actions">
                            <For each={["main", "remote", "delete", "edited_text"] as const}>
                              {(choice) => (
                                <Button
                                  variant={choices()[path.path] === choice ? "primary" : "outline"}
                                  size="sm"
                                  onClick={() =>
                                    setChoices((current) => ({ ...current, [path.path]: choice }))
                                  }
                                >
                                  {choice}
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
                      disabled={busy()}
                      onClick={() =>
                        void run(async () => {
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
                        }, "Conflict resolved")
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
          {props.title} <span class="files-editor-meta">({props.count})</span>
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
  return (
    <li class="scm-path-item">
      <input
        type="checkbox"
        checked={props.selected}
        aria-label={`Select ${props.path}`}
        onChange={props.onToggle}
      />
      <button type="button" class="scm-path-open" title={props.path} onClick={props.onOpen}>
        <span class="scm-path-name" title={props.path}>
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
      >
        {props.letter}
      </span>
    </li>
  );
}
