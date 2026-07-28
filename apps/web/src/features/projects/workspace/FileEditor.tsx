import { useQueryClient } from "@tanstack/solid-query";
import FileCode2 from "lucide-solid/icons/file-code-2";
import Save from "lucide-solid/icons/save";
import { createEffect, createSignal, Show } from "solid-js";
import { produce } from "solid-js/store";
import { Button } from "../../../components/ui/Button";
import { EmptyState } from "../../../components/ui/EmptyState";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import { useNotifications } from "../../../components/ui/notifications";
import { getFileContentText, getFileMeta, saveFileText } from "../../../lib/api";
import type { FileDocument } from "./workspaceState";

/**
 * Controlled single-file editor. The ProjectPage owns the tab store; this
 * component renders one file tab's surface and reports draft/save mutations
 * through `onPatch`. Draft state lives in the owned tab so switching or
 * closing tabs never loses unsaved input.
 */
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

  const isDirty = () =>
    Boolean(props.tab().meta?.editable) &&
    props.tab().draft !== props.tab().saved &&
    !props.tab().loading;

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
          tab.loadError = error instanceof Error ? error.message : "Failed to load file";
        }),
      );
    }
  }

  async function savePath(path: string): Promise<boolean> {
    const id = props.projectId();
    const tab = props.tab();
    if (!id || !tab.meta?.editable) return false;

    setSaving(true);
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
      const message = error instanceof Error ? error.message : "Save failed";
      props.onPatch((item) => {
        item.loadError = message;
      });
      notify(message, { variant: "danger" });
      return false;
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="ide-editor-surface">
      <Show
        when={props.tab().meta || !props.tab().loading}
        fallback={<p class="surface-note">Loading…</p>}
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
          </div>
          <Button
            variant="primary"
            size="sm"
            disabled={!props.tab().meta?.editable || !isDirty() || saving() || props.tab().loading}
            onClick={() => void savePath(props.tab().path)}
          >
            <Save size={14} />
            Save
          </Button>
        </div>
        <Show when={props.tab().loadError}>
          <ErrorBlock variant="inline" message={props.tab().loadError} />
        </Show>
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
          />
        </Show>
      </Show>
    </div>
  );
}
