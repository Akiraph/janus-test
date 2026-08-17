import FileCode2 from "lucide-solid/icons/file-code-2";
import Files from "lucide-solid/icons/files";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import PanelLeftOpen from "lucide-solid/icons/panel-left-open";
import X from "lucide-solid/icons/x";
import { For, Show, Suspense } from "solid-js";
import { EmptyState } from "../../../components/ui/EmptyState";
import type { ProjectView } from "../../../lib/api";
import { basename } from "../../../lib/utils";
import { FileEditor } from "../../file-editor/FileEditor";
import { SessionTabView } from "../../session/SessionTabView";
import type {
  FileDocument,
  SessionDocument,
  WorkspaceDocument,
  WorkspaceState,
} from "./workspaceState";

interface WorkspaceDocumentsProps {
  workspace: WorkspaceState;
  projectId: () => string | undefined;
  project: () => ProjectView | undefined;
  ready: () => boolean;
  sessionCreating: () => boolean;
  onFileSaved: (projectId: string) => void | Promise<void>;
}

export function WorkspaceDocuments(props: WorkspaceDocumentsProps) {
  const activeReadyDocument = () => (props.ready() ? props.workspace.activeDocument() : undefined);

  return (
    <main class="ide-main">
      <Show when={props.workspace.documents.length > 0 || !props.workspace.navigationOpen()}>
        <div class="ide-main-tabs">
          <Show when={!props.workspace.navigationOpen()}>
            <button
              type="button"
              class="workspace-navigation-button"
              aria-label="Open workspace navigation"
              title="Open workspace navigation"
              onClick={props.workspace.openNavigation}
            >
              <PanelLeftOpen size={16} />
            </button>
          </Show>
          <div class="ide-tabs" role="tablist" aria-label="Open documents">
            <For each={props.workspace.documents}>
              {(document) => (
                <DocumentTab
                  document={document}
                  active={props.workspace.activeDocumentId() === document.id}
                  onActivate={() => props.workspace.activateDocument(document.id)}
                  onClose={() => props.workspace.closeDocument(document.id)}
                />
              )}
            </For>
          </div>
        </div>
      </Show>

      <div class="ide-main-surface">
        <Show
          when={activeReadyDocument()}
          fallback={
            <Show
              when={props.ready()}
              fallback={
                <Show
                  when={props.project()}
                  fallback={
                    <EmptyState
                      icon={Files}
                      title="Opening workspace…"
                      description="Connecting to the project."
                    />
                  }
                >
                  {(project) => (
                    <EmptyState
                      icon={Files}
                      title={`Project is ${project().state}`}
                      description={
                        project().state === "creating"
                          ? "Clone is still running. Files and Git unlock when the project is ready."
                          : "This project is not ready for workspace tools yet."
                      }
                    />
                  )}
                </Show>
              }
            >
              <EmptyState
                icon={FileCode2}
                title="No document open"
                description="Open a Session from Sessions, or a file from Explorer."
              />
            </Show>
          }
        >
          {(document) => (
            <Suspense
              fallback={
                <div class="ide-shell-scaffold-loading" role="status" aria-label="Loading">
                  <Loader2 size={22} class="ui-spinner" />
                </div>
              }
            >
              <Show
                when={document().kind === "session"}
                fallback={
                  <FileEditor
                    projectId={props.projectId}
                    mainRevision={() => props.project()?.main_revision ?? null}
                    tab={() => document() as FileDocument}
                    onPatch={(update) => props.workspace.updateFile(document().id, update)}
                    onSaved={props.onFileSaved}
                  />
                }
              >
                <SessionTabView
                  sessionId={() => (document() as SessionDocument).sessionId}
                  creating={props.sessionCreating}
                  subView={() => (document() as SessionDocument).subView}
                  onSubViewChange={(subView) =>
                    props.workspace.updateSession(document().id, (session) => {
                      session.subView = subView;
                    })
                  }
                  onTitle={(title) =>
                    props.workspace.updateSession(document().id, (session) => {
                      session.title = title;
                    })
                  }
                />
              </Show>
            </Suspense>
          )}
        </Show>
      </div>
    </main>
  );
}

function DocumentTab(props: {
  document: WorkspaceDocument;
  active: boolean;
  onActivate: () => void;
  onClose: () => void;
}) {
  const label = () =>
    props.document.kind === "file"
      ? basename(props.document.path)
      : props.document.title || "Session";
  const title = () =>
    props.document.kind === "file" ? props.document.path : `Session ${props.document.sessionId}`;
  const dirty = () =>
    props.document.kind === "file" &&
    Boolean(props.document.meta?.editable) &&
    props.document.draft !== props.document.saved;

  return (
    <div
      class="ide-tab"
      classList={{
        "ide-tab--active": props.active,
        "ide-tab--dirty": dirty(),
        "ide-tab--session": props.document.kind === "session",
      }}
    >
      <button
        type="button"
        class="ide-tab-label"
        role="tab"
        aria-selected={props.active}
        aria-label={`${label()}${dirty() ? ", unsaved changes" : ""}${props.document.kind === "session" ? " (Session)" : ""}`}
        title={title()}
        onClick={props.onActivate}
      >
        <Show when={props.document.kind === "session"}>
          <MessageSquare size={12} class="ide-tab-kind-icon" />
        </Show>
        <Show when={dirty()}>
          <span class="ide-tab-dirty" aria-hidden="true" title="Unsaved changes" />
        </Show>
        <span>{label()}</span>
      </button>
      <button
        type="button"
        class="ide-tab-close"
        aria-label={`Close ${label()}`}
        onClick={(event) => {
          event.stopPropagation();
          props.onClose();
        }}
      >
        <X size={12} />
      </button>
    </div>
  );
}
