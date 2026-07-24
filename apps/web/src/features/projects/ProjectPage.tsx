import { A, useParams } from "@solidjs/router";
import { useQueryClient } from "@tanstack/solid-query";
import { ArrowLeft, FileCode2, Folder, GitBranch, Save, TerminalSquare } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { ErrorBlock } from "../../components/ui/ErrorBlock";
import { useNotifications } from "../../components/ui/notifications";
import { Skeleton } from "../../components/ui/Skeleton";
import { type TabItem, Tabs } from "../../components/ui/Tabs";
import type { FileMetaView, FileTreeView } from "../../lib/api";
import {
  getFileContentText,
  getFileMeta,
  gitCommit,
  gitFetch,
  gitPush,
  gitRemotes,
  gitStage,
  gitUnstage,
  saveFileText,
} from "../../lib/api";
import { useFileTree, useGitLog, useGitStatus, useProject } from "../../lib/queries";

const PROJECT_TABS: TabItem[] = [
  { value: "files", label: "Files" },
  { value: "git", label: "Git" },
  { value: "terminal", label: "Terminal" },
];

function basename(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function parentPath(path: string): string {
  const parts = path.split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}

export function ProjectPage() {
  const params = useParams<{ id: string }>();
  const projectId = () => params.id;
  const project = useProject(projectId);
  const [tab, setTab] = createSignal("files");

  return (
    <section class="project-page route-enter" aria-labelledby="project-title">
      <div class="project-page-header">
        <A class="project-back" href="/">
          <ArrowLeft size={16} />
          Projects
        </A>
        <Show
          when={!project.isPending}
          fallback={<Skeleton class="project-header-skeleton" compact />}
        >
          <Show
            when={project.data}
            fallback={
              <ErrorBlock
                message={
                  project.error instanceof Error ? project.error.message : "Project not found"
                }
                retry={() => void project.refetch()}
              />
            }
          >
            {(data) => (
              <div class="project-title-row">
                <div>
                  <h1 id="project-title">{data().name}</h1>
                  <p class="project-subtitle">
                    {data().repository.url}
                    <Show when={data().current_branch ?? data().repository.branch}>
                      {(branch) => (
                        <>
                          {" · "}
                          <GitBranch size={12} class="inline-icon" /> {branch()}
                        </>
                      )}
                    </Show>
                  </p>
                </div>
                <Badge
                  variant={
                    data().state === "ready"
                      ? "success"
                      : data().state === "error"
                        ? "danger"
                        : "warning"
                  }
                >
                  {data().state}
                </Badge>
              </div>
            )}
          </Show>
        </Show>
        <Tabs value={tab()} onChange={setTab} tabs={PROJECT_TABS} aria-label="Project sections" />
      </div>

      <Show
        when={project.data?.state === "ready"}
        fallback={
          <Show when={project.data}>
            {(data) => (
              <EmptyState
                icon={Folder}
                title={`Project is ${data().state}`}
                description={
                  data().state === "creating"
                    ? "Clone is still running. Files and Git unlock when the project is ready."
                    : "This project is not ready for workspace tools yet."
                }
              />
            )}
          </Show>
        }
      >
        <Show when={tab() === "files"}>
          <FilesTab
            projectId={projectId}
            mainRevision={() => project.data?.main_revision ?? null}
          />
        </Show>
        <Show when={tab() === "git"}>
          <GitTab projectId={projectId} />
        </Show>
        <Show when={tab() === "terminal"}>
          <TerminalTab />
        </Show>
      </Show>
    </section>
  );
}

function FilesTab(props: {
  projectId: () => string | undefined;
  mainRevision: () => string | null;
}) {
  const notify = useNotifications().notify;
  const queryClient = useQueryClient();
  const [dir, setDir] = createSignal("");
  const tree = useFileTree(props.projectId, dir);
  const [selectedPath, setSelectedPath] = createSignal<string | null>(null);
  const [meta, setMeta] = createSignal<FileMetaView | null>(null);
  const [content, setContent] = createSignal("");
  const [draft, setDraft] = createSignal("");
  const [loadError, setLoadError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const dirty = createMemo(() => draft() !== content());

  const sortedEntries = createMemo(() => {
    const entries = [...(tree.data ?? [])];
    return entries.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
      return a.path.localeCompare(b.path);
    });
  });

  createEffect(() => {
    const path = selectedPath();
    const id = props.projectId();
    if (!path || !id) {
      setMeta(null);
      setContent("");
      setDraft("");
      setLoadError("");
      return;
    }

    let cancelled = false;
    setLoadError("");
    void (async () => {
      try {
        const nextMeta = await getFileMeta(id, path);
        if (cancelled) return;
        setMeta(nextMeta);
        if (!nextMeta.editable) {
          setContent("");
          setDraft("");
          return;
        }
        const text = await getFileContentText(id, path);
        if (cancelled) return;
        setContent(text);
        setDraft(text);
      } catch (error) {
        if (cancelled) return;
        setLoadError(error instanceof Error ? error.message : "Failed to load file");
        setMeta(null);
        setContent("");
        setDraft("");
      }
    })();

    onCleanup(() => {
      cancelled = true;
    });
  });

  async function openEntry(entry: FileTreeView) {
    if (entry.kind === "dir") {
      setDir(entry.path);
      setSelectedPath(null);
      return;
    }
    setSelectedPath(entry.path);
  }

  async function save() {
    const id = props.projectId();
    const path = selectedPath();
    if (!id || !path) return;
    setSaving(true);
    setLoadError("");
    try {
      await saveFileText(id, {
        path,
        content: draft(),
        expected_main_revision: props.mainRevision(),
      });
      setContent(draft());
      notify("File saved", { variant: "success" });
      await queryClient.invalidateQueries({ queryKey: ["project", id] });
      await queryClient.invalidateQueries({ queryKey: ["file-tree", id] });
      const nextMeta = await getFileMeta(id, path);
      setMeta(nextMeta);
    } catch (error) {
      const message = error instanceof Error ? error.message : "Save failed";
      setLoadError(message);
      notify(message, { variant: "danger" });
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="files-layout">
      <aside class="files-tree" aria-label="File tree">
        <div class="files-tree-toolbar">
          <Button
            variant="ghost"
            size="sm"
            disabled={!dir()}
            onClick={() => setDir(parentPath(dir()))}
          >
            Up
          </Button>
          <span class="files-tree-path">{dir() || "/"}</span>
        </div>
        <Show when={!tree.isPending} fallback={<Skeleton compact />}>
          <Show
            when={!tree.isError}
            fallback={
              <ErrorBlock
                message={tree.error instanceof Error ? tree.error.message : "Tree failed"}
                retry={() => void tree.refetch()}
              />
            }
          >
            <Show
              when={(sortedEntries().length ?? 0) > 0}
              fallback={<p class="files-tree-empty">Empty directory</p>}
            >
              <ul class="files-tree-list">
                <For each={sortedEntries()}>
                  {(entry) => (
                    <li>
                      <button
                        type="button"
                        class="files-tree-item"
                        classList={{
                          "files-tree-item--active": selectedPath() === entry.path,
                          "files-tree-item--dir": entry.kind === "dir",
                        }}
                        onClick={() => void openEntry(entry)}
                      >
                        {entry.kind === "dir" ? <Folder size={14} /> : <FileCode2 size={14} />}
                        <span>{basename(entry.path)}</span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
        </Show>
      </aside>

      <div class="files-editor">
        <Show
          when={selectedPath()}
          fallback={
            <EmptyState
              icon={FileCode2}
              title="Select a file"
              description="Browse the tree to open a text file in the Main Workspace editor."
            />
          }
        >
          <div class="files-editor-toolbar">
            <div>
              <strong>{selectedPath()}</strong>
              <Show when={meta()}>
                {(value) => (
                  <p class="files-editor-meta">
                    {value().editable ? "Editable" : "Not editable"} · {value().size} bytes
                  </p>
                )}
              </Show>
            </div>
            <Button
              variant="primary"
              size="sm"
              disabled={!meta()?.editable || !dirty() || saving()}
              onClick={() => void save()}
            >
              <Save size={14} />
              Save
            </Button>
          </div>
          <Show when={loadError()}>
            <ErrorBlock variant="inline" message={loadError()} />
          </Show>
          <Show
            when={meta()?.editable}
            fallback={
              <EmptyState
                icon={FileCode2}
                title="File not editable"
                description="Binary, oversized, or non-UTF-8 files can be downloaded via the API but not edited here."
              />
            }
          >
            {/* M2 uses a native monospace textarea; CodeMirror is not in package.json. */}
            <textarea
              class="files-textarea"
              value={draft()}
              spellcheck={false}
              aria-label="File content"
              onInput={(event) => setDraft(event.currentTarget.value)}
            />
          </Show>
        </Show>
      </div>
    </div>
  );
}

function GitTab(props: { projectId: () => string | undefined }) {
  const notify = useNotifications().notify;
  const queryClient = useQueryClient();
  const status = useGitStatus(props.projectId);
  const log = useGitLog(props.projectId, 20);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);

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
    await queryClient.invalidateQueries({ queryKey: ["git-status", id] });
    await queryClient.invalidateQueries({ queryKey: ["git-log", id] });
  }

  async function run(action: () => Promise<void>, success: string) {
    setBusy(true);
    try {
      await action();
      notify(success, { variant: "success" });
      setSelected(new Set<string>());
      await refreshGit();
    } catch (error) {
      notify(error instanceof Error ? error.message : "Git command failed", {
        variant: "danger",
      });
    } finally {
      setBusy(false);
    }
  }

  const selectedPaths = () => [...selected()];

  return (
    <div class="git-layout">
      <Show when={!status.isPending} fallback={<Skeleton />}>
        <Show
          when={status.data}
          fallback={
            <ErrorBlock
              message={status.error instanceof Error ? status.error.message : "Git status failed"}
              retry={() => void status.refetch()}
            />
          }
        >
          {(data) => (
            <>
              <div class="git-summary">
                <Badge variant="neutral">
                  {data().branch ?? "detached"}
                  {data().head_sha ? ` · ${data().head_sha?.slice(0, 7)}` : ""}
                </Badge>
                <span>
                  ↑{data().ahead} ↓{data().behind}
                </span>
                <div class="git-actions">
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy() || selectedPaths().length === 0}
                    onClick={() =>
                      void run(
                        () => gitStage(props.projectId() as string, selectedPaths()),
                        "Staged",
                      )
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
                        await gitPush(id, remote, branch, crypto.randomUUID());
                      }, "Push started")
                    }
                  >
                    Push
                  </Button>
                </div>
              </div>

              <div class="git-sections">
                <GitPathSection
                  title="Working tree"
                  paths={data().working}
                  selected={selected()}
                  onToggle={toggle}
                />
                <GitPathSection
                  title="Index"
                  paths={data().index}
                  selected={selected()}
                  onToggle={toggle}
                />
                <GitPathSection
                  title="Untracked"
                  paths={data().untracked}
                  selected={selected()}
                  onToggle={toggle}
                />
              </div>

              <div class="git-commit">
                <span class="field-label">Commit message</span>
                <textarea
                  class="ui-input git-commit-message"
                  value={message()}
                  onInput={(event) => setMessage(event.currentTarget.value)}
                  placeholder="Describe the change"
                  aria-label="Commit message"
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
            </>
          )}
        </Show>
      </Show>

      <section class="git-log" aria-label="Recent commits">
        <h2>Recent commits</h2>
        <Show when={!log.isPending} fallback={<Skeleton compact />}>
          <Show
            when={(log.data?.length ?? 0) > 0}
            fallback={<p class="files-tree-empty">No commits yet</p>}
          >
            <ul class="git-log-list">
              <For each={log.data}>
                {(entry) => (
                  <li>
                    <code>{entry.sha.slice(0, 7)}</code>
                    <div>
                      <strong>{entry.message}</strong>
                      <p>{entry.author}</p>
                    </div>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </section>
    </div>
  );
}

function GitPathSection(props: {
  title: string;
  paths: string[];
  selected: Set<string>;
  onToggle: (path: string) => void;
}) {
  return (
    <section class="git-path-section">
      <h3>
        {props.title}
        <Badge variant="neutral">{props.paths.length}</Badge>
      </h3>
      <Show when={props.paths.length > 0} fallback={<p class="files-tree-empty">Clean</p>}>
        <ul class="git-path-list">
          <For each={props.paths}>
            {(path) => (
              <li>
                <label class="git-path-row">
                  <input
                    class="ui-checkbox"
                    type="checkbox"
                    checked={props.selected.has(path)}
                    onChange={() => props.onToggle(path)}
                  />
                  <span>{path}</span>
                </label>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

function TerminalTab() {
  return (
    <EmptyState
      icon={TerminalSquare}
      title="Terminal unavailable"
      description="Main Workspace Terminal depends on RuntimeExecutor and lands in M4. This tab is a placeholder for M2."
      class="terminal-placeholder"
    />
  );
}
