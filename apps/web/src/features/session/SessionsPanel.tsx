import { useQueryClient } from "@tanstack/solid-query";
import Check from "lucide-solid/icons/check";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import Plus from "lucide-solid/icons/plus";
import Trash2 from "lucide-solid/icons/trash-2";
import { createEffect, createSignal, For, Show } from "solid-js";
import { Alt } from "../../components/ui/Alt";
import { EmptyState } from "../../components/ui/EmptyState";
import { useNotifications } from "../../components/ui/notifications";
import { SideScrollbar } from "../../components/ui/SideScrollbar";
import type { SessionSummary } from "../../lib/api";
import { createSession, deleteSession, getErrorMessage, getSession } from "../../lib/api";
import { useOperation, useSessions } from "../../lib/queries";
import "../projects/workspace/sessions-panel.css";

interface SessionsPanelProps {
  projectId: () => string | undefined;
  projectReady?: () => boolean;
  /** Currently focused session tab id, for highlight. */
  activeSessionId?: () => string | null;
  creating: () => boolean;
  onCreatingChange: (creating: boolean) => void;
  /** Open (or focus) a session as a project main-area tab. */
  onOpenSession: (sessionId: string, title?: string | null) => void;
  /** Notify the host to drop any open tab for a session that was deleted here. */
  onSessionDeleted?: (sessionId: string) => void;
}

type SessionOperation =
  | {
      operationId: string;
      kind: "create";
      projectId: string;
      sessionId: string;
    }
  | {
      operationId: string;
      kind: "delete";
      session: SessionSummary;
    };

export function SessionsPanel(props: SessionsPanelProps) {
  const sessions = useSessions(props.projectId);
  const queryClient = useQueryClient();
  const notify = useNotifications().notify;
  const [deletingId, setDeletingId] = createSignal<string | null>(null);
  const [sessionOperation, setSessionOperation] = createSignal<SessionOperation | null>(null);
  const [scrollHost, setScrollHost] = createSignal<HTMLElement | null>(null);
  const trackedOperation = useOperation(() => sessionOperation()?.operationId);
  let handledOperationId: string | null = null;

  createEffect(() => {
    const current = sessionOperation();
    const operation = trackedOperation.data;
    if (
      !current ||
      !operation ||
      operation.id !== current.operationId ||
      handledOperationId === operation.id
    ) {
      return;
    }

    if (operation.status !== "succeeded") {
      if (
        operation.status === "failed" ||
        operation.status === "canceled" ||
        operation.status === "needs_attention"
      ) {
        handledOperationId = operation.id;
        setSessionOperation(null);
        if (current.kind === "create") {
          queryClient.removeQueries({ queryKey: ["session", current.sessionId] });
          queryClient.setQueryData<SessionSummary[]>(["sessions", current.projectId], (prev) =>
            (prev ?? []).filter((candidate) => candidate.id !== current.sessionId),
          );
          props.onSessionDeleted?.(current.sessionId);
          props.onCreatingChange(false);
          notify(getErrorMessage(operation.problem, "Failed to create session"), {
            variant: "danger",
          });
        } else {
          queryClient.setQueryData<SessionSummary[]>(
            ["sessions", current.session.project_id],
            (prev) => {
              const list = prev ?? [];
              return list.some((candidate) => candidate.id === current.session.id)
                ? list
                : [current.session, ...list];
            },
          );
          setDeletingId(null);
          notify(getErrorMessage(operation.problem, "Failed to delete session"), {
            variant: "danger",
          });
        }
      }
      return;
    }

    handledOperationId = operation.id;
    if (current.kind === "create") {
      const durableSessionId = operation.target_id ?? current.sessionId;
      void (async () => {
        try {
          const cached = queryClient.getQueryData<SessionSummary>(["session", durableSessionId]);
          const session = cached?.state === "ready" ? cached : await getSession(durableSessionId);
          queryClient.setQueryData(["session", session.id], session);
          queryClient.setQueryData(["session-timeline", session.id], {
            items: [],
            has_older: false,
            has_newer: false,
            oldest_cursor: null,
            newest_cursor: null,
          });
          queryClient.setQueryData<SessionSummary[]>(["sessions", current.projectId], (prev) => {
            const list = prev ?? [];
            if (list.some((candidate) => candidate.id === session.id)) {
              return list.map((candidate) => (candidate.id === session.id ? session : candidate));
            }
            return [session, ...list];
          });
          void queryClient.invalidateQueries({ queryKey: ["session-context", session.id] });
        } catch (error) {
          queryClient.removeQueries({ queryKey: ["session", current.sessionId] });
          queryClient.setQueryData<SessionSummary[]>(["sessions", current.projectId], (prev) =>
            (prev ?? []).filter((candidate) => candidate.id !== current.sessionId),
          );
          props.onSessionDeleted?.(current.sessionId);
          notify(getErrorMessage(error, "Failed to create session"), {
            variant: "danger",
          });
        } finally {
          setSessionOperation(null);
          props.onCreatingChange(false);
        }
      })();
      return;
    }

    queryClient.removeQueries({ queryKey: ["session", current.session.id] });
    setSessionOperation(null);
    setDeletingId(null);
  });

  async function onCreate() {
    const id = props.projectId();
    if (!id || props.creating()) return;
    if (props.projectReady && !props.projectReady()) return;
    props.onCreatingChange(true);
    try {
      const accepted = await createSession(id, { title: "New session" }, crypto.randomUUID());
      const sessionId = accepted.target_id;
      if (!sessionId) throw new Error("Session creation was accepted without a Session id");
      queryClient.setQueryData(["operations", accepted.id], accepted);
      const now = new Date().toISOString();
      const optimisticSession: SessionSummary = {
        id: sessionId,
        project_id: id,
        title: "New session",
        state: "creating",
        active_turn_id: null,
        version: "",
        model_preference: null,
        created_at: now,
        updated_at: now,
        last_activity_at: now,
      };
      queryClient.setQueryData<SessionSummary>(["session", sessionId], optimisticSession);
      queryClient.setQueryData<SessionSummary[]>(["sessions", id], (prev) => [
        optimisticSession,
        ...(prev ?? []).filter((candidate) => candidate.id !== sessionId),
      ]);
      queryClient.setQueryData(["session-timeline", sessionId], {
        items: [],
        has_older: false,
        has_newer: false,
        oldest_cursor: null,
        newest_cursor: null,
      });
      // Open the durable document immediately. The workspace operation can
      // continue preparing the Git worktree while the document shows its
      // creating state with the composer disabled.
      props.onOpenSession(sessionId, optimisticSession.title);
      setSessionOperation({ operationId: accepted.id, kind: "create", projectId: id, sessionId });
    } catch (error) {
      // Session create/delete are sidebar actions — failures surface as a
      // transient toast, not a red block that occupies the session list.
      notify(getErrorMessage(error, "Failed to create session"), {
        variant: "danger",
      });
      props.onCreatingChange(false);
    }
  }

  async function onDelete(session: SessionSummary) {
    if (deletingId()) return;
    setDeletingId(session.id);
    try {
      const accepted = await deleteSession(session.id, session.version, crypto.randomUUID());
      queryClient.setQueryData(["operations", accepted.id], accepted);
      props.onSessionDeleted?.(session.id);
      queryClient.setQueryData<SessionSummary[]>(["sessions", session.project_id], (prev) =>
        (prev ?? []).filter((candidate) => candidate.id !== session.id),
      );
      setSessionOperation({ operationId: accepted.id, kind: "delete", session });
    } catch (error) {
      notify(getErrorMessage(error, "Failed to delete session"), {
        variant: "danger",
      });
      setDeletingId(null);
    }
  }

  return (
    <div class="ide-sidebar-panel sessions-panel">
      <div class="ide-sidebar-header">
        <span>Sessions</span>
        <button
          type="button"
          class="sessions-panel__new"
          title={props.creating() ? "Creating session..." : "New session"}
          disabled={props.creating() || (props.projectReady ? !props.projectReady() : false)}
          onClick={() => void onCreate()}
        >
          <Show when={props.creating()} fallback={<Plus size={14} />}>
            <Loader2 size={14} class="ui-spinner" />
          </Show>
        </button>
      </div>

      <div class="ide-scroll-host sessions-panel__scroll">
        <div class="ide-sidebar-scroll sessions-panel__list" ref={setScrollHost}>
          <Show when={!sessions.isLoading} fallback={<p class="sessions-panel__hint">Loading…</p>}>
            <Show
              when={(sessions.data?.length ?? 0) > 0}
              fallback={
                <EmptyState
                  icon={MessageSquare}
                  title="No sessions"
                  description="Create a Session to work with the Supervisor in this project's Main workspace."
                  class="sessions-panel__empty"
                />
              }
            >
              <ul class="sessions-panel__items">
                <For each={sessions.data ?? []}>
                  {(session) => (
                    <li>
                      <SessionRow
                        session={session}
                        active={() => props.activeSessionId?.() === session.id}
                        deleting={() => deletingId() === session.id}
                        onOpen={() => props.onOpenSession(session.id, session.title)}
                        onDelete={() => void onDelete(session)}
                      />
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
        </div>
        <SideScrollbar host={scrollHost} />
      </div>
    </div>
  );
}

function SessionRow(props: {
  session: SessionSummary;
  active: () => boolean;
  deleting: () => boolean;
  onOpen: () => void;
  onDelete: () => void;
}) {
  const status = createStatusInfo(props.session.state);
  return (
    <div
      class="sessions-panel__item"
      classList={{
        "sessions-panel__item--active": props.active(),
        "sessions-panel__item--creating": props.session.state === "creating",
      }}
      title={props.session.title ?? "Untitled session"}
    >
      <button
        type="button"
        class="sessions-panel__select"
        disabled={props.session.state === "creating" || props.deleting()}
        onClick={props.onOpen}
      >
        <Alt content={status.label} class="alt-bubble">
          <span class="sessions-panel__status" aria-hidden="true">
            <Show when={status.spinning} fallback={status.icon}>
              <Loader2 size={14} class="ui-spinner" />
            </Show>
          </span>
        </Alt>
        <span class="sessions-panel__title">{props.session.title ?? "Untitled session"}</span>
      </button>
      {/* Keep the secondary row action quiet until the row receives attention. */}
      <button
        type="button"
        class="sessions-panel__action"
        title="Delete session"
        aria-label={`Delete session ${props.session.title ?? props.session.id}`}
        disabled={props.deleting() || props.session.state === "creating"}
        onClick={(event) => {
          event.stopPropagation();
          props.onDelete();
        }}
      >
        <Show when={props.deleting()} fallback={<Trash2 size={14} />}>
          <Loader2 size={14} class="ui-spinner" />
        </Show>
      </button>
    </div>
  );
}

/** Map a server Session state to the icon and label used by the list row. */
function createStatusInfo(state: string): {
  icon: import("solid-js").JSX.Element;
  label: string;
  spinning: boolean;
} {
  switch (state) {
    case "creating":
      return { icon: null, label: "Creating...", spinning: true };
    case "active":
      return { icon: null, label: "Active (turn running)", spinning: true };
    case "deleting":
      return { icon: null, label: "Deleting…", spinning: true };
    case "ready":
      return { icon: <Check size={14} />, label: "Ready", spinning: false };
    default:
      return { icon: <MessageSquare size={14} />, label: state || "Idle", spinning: false };
  }
}
