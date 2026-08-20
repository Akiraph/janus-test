import { useQueryClient } from "@tanstack/solid-query";
import FileCode2 from "lucide-solid/icons/file-code-2";
import Loader2 from "lucide-solid/icons/loader-2";
import Save from "lucide-solid/icons/save";
import { createEffect, createSignal, on, onCleanup, Show } from "solid-js";
import { produce } from "solid-js/store";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { NotificationEvent, useNotifications } from "../../components/ui/notifications";
import {
  ApiError,
  getErrorMessage,
  getFileContentText,
  getFileMeta,
  saveFileText,
} from "../../lib/api";
import type { FileDocument } from "../projects/workspace/workspaceState";
import "./files.css";

interface FileEditorProps {
  projectId: () => string | undefined;
  mainRevision: () => string | null;
  tab: () => FileDocument;
  onPatch: (mutator: (tab: FileDocument) => void) => void;
  onSaved: (projectId: string) => void | Promise<void>;
}

export function FileEditor(props: FileEditorProps) {
  const notify = useNotifications().notify;
  const queryClient = useQueryClient();
  const [saving, setSaving] = createSignal(false);
  const [saveError, setSaveError] = createSignal("");

  const isDirty = () =>
    Boolean(props.tab().meta?.editable) &&
    props.tab().draft !== props.tab().saved &&
    !props.tab().loading;

  // A beforeunload listener is the only way the browser will warn about a
  // reload/close that would drop the draft, so keep one live while dirty.
  createEffect(() => {
    if (!isDirty()) return;
    const guard = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", guard);
    onCleanup(() => window.removeEventListener("beforeunload", guard));
  });

  // One editor instance serves every file tab, so a previous tab's failure must
  // not follow the user to the next file.
  createEffect(
    on(
      () => props.tab().path,
      () => setSaveError(""),
    ),
  );

  // Load file content on demand. Re-runs when the tab's path changes and
  // whenever metadata has not been fetched yet.
  createEffect(() => {
    const id = props.projectId();
    const tab = props.tab();
    if (!id || !tab.path) return;
    if (tab.meta || tab.loading || tab.loadError) return;
    void loadInto(tab.path);
  });

  async function loadInto(path: string) {
    const id = props.projectId();
    if (!id) return;
    props.onPatch((tab) => {
      tab.loading = true;
    });
    try {
      const meta = await getFileMeta(id, path);
      if (!meta.editable) {
        props.onPatch(
          produce((tab) => {
            tab.meta = meta;
            tab.loading = false;
            tab.loadError = "";
          }),
        );
        return;
      }
      const text = await getFileContentText(id, path);
      props.onPatch(
        produce((tab) => {
          tab.draft = text;
          tab.saved = text;
          tab.meta = meta;
          tab.loading = false;
          tab.loadError = "";
        }),
      );
    } catch (error) {
      props.onPatch(
        produce((tab) => {
          tab.loading = false;
          tab.loadError = getErrorMessage(error, "Failed to load file");
        }),
      );
    }
  }

  async function savePath(path: string): Promise<boolean> {
    const id = props.projectId();
    const tab = props.tab();
    if (!id || !tab.meta?.editable) return false;

    setSaving(true);
    setSaveError("");
    try {
      await saveFileText(id, {
        path,
        content: tab.draft,
        expected_main_revision: props.mainRevision(),
      });
      props.onPatch((item) => {
        item.saved = item.draft;
      });
      const nextMeta = await getFileMeta(id, path);
      props.onPatch((item) => {
        item.meta = nextMeta;
      });
      await queryClient.invalidateQueries({ queryKey: ["project", id] });
      await queryClient.invalidateQueries({ queryKey: ["file-tree", id] });
      await queryClient.invalidateQueries({ queryKey: ["git-status", id] });
      await props.onSaved(id);
      notify("File saved", { variant: "success" });
      return true;
    } catch (error) {
      const message = saveFailureMessage(error);
      setSaveError(message);
      notify(message, { variant: "danger" });
      return false;
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="ide-editor-surface">
      <NotificationEvent
        message={props.tab().loadError}
        variant="danger"
        action={{
          label: "Retry",
          onClick: () =>
            props.onPatch((tab) => {
              tab.loadError = "";
            }),
        }}
      />
      <Show
        when={props.tab().meta || !props.tab().loading}
        fallback={
          <p class="surface-note" role="status">
            Loading…
          </p>
        }
      >
        <div class="files-editor-toolbar">
          <div>
            <strong>{props.tab().path}</strong>
            <Show when={props.tab().meta}>
              {(meta) => (
                <p class="files-editor-meta">
                  {meta().editable ? "Editable" : "Not editable"} · {meta().size} bytes
                  <Show when={isDirty()}> · Unsaved</Show>
                </p>
              )}
            </Show>
            <Show when={saveError()}>
              <p class="files-editor-error" role="alert">
                {saveError()}
              </p>
            </Show>
          </div>
          <Button
            variant="primary"
            size="sm"
            disabled={!props.tab().meta?.editable || !isDirty() || saving() || props.tab().loading}
            title={saveButtonHint(props.tab().meta?.editable ?? false, isDirty(), saving())}
            onClick={() => void savePath(props.tab().path)}
          >
            <Show when={saving()} fallback={<Save size={14} />}>
              <Loader2 size={14} class="ui-spinner" aria-hidden="true" />
            </Show>
            {saving() ? "Saving…" : "Save"}
          </Button>
        </div>
        <Show when={!props.tab().loading && props.tab().meta && !props.tab().meta?.editable}>
          <EmptyState
            icon={FileCode2}
            title="File not editable"
            description="Binary, oversized, or non-UTF-8 files can be downloaded via the API but not edited here."
          />
        </Show>
        <Show when={!props.tab().loading && props.tab().meta?.editable}>
          <textarea
            class="files-textarea ide-textarea"
            value={props.tab().draft}
            spellcheck={false}
            aria-label={`File content ${props.tab().path}`}
            onInput={(event) => {
              const value = event.currentTarget.value;
              props.onPatch((item) => {
                item.draft = value;
              });
            }}
            onKeyDown={(event) => {
              if (event.key.toLowerCase() !== "s" || !(event.ctrlKey || event.metaKey)) return;
              event.preventDefault();
              if (isDirty() && !saving()) void savePath(props.tab().path);
            }}
          />
        </Show>
      </Show>
    </div>
  );
}

function saveButtonHint(editable: boolean, dirty: boolean, saving: boolean): string {
  if (!editable) return "This file cannot be edited here";
  if (saving) return "Saving…";
  if (!dirty) return "No unsaved changes";
  return "Save file (Ctrl+S)";
}

function saveFailureMessage(error: unknown): string {
  if (error instanceof ApiError && error.code === "RESOURCE_VERSION_MISMATCH") {
    return "Save failed: the file changed since you opened it. Reopen it, then redo your edit.";
  }
  return `Save failed: ${getErrorMessage(error, "the server rejected the write")}`;
}
