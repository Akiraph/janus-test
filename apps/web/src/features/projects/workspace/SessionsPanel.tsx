import { useQueryClient } from "@tanstack/solid-query";
import Check from "lucide-solid/icons/check";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import Plus from "lucide-solid/icons/plus";
import Trash2 from "lucide-solid/icons/trash-2";
import { createSignal, For, Show } from "solid-js";
import { Alt } from "../../../components/ui/Alt";
import { EmptyState } from "../../../components/ui/EmptyState";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import { SideScrollbar } from "../../../components/ui/SideScrollbar";
import type { SessionSummary } from "../../../lib/api";
import { createSession, deleteSession, getSession, waitForOperation } from "../../../lib/api";
import { useSessions } from "../../../lib/queries";

interface SessionsPanelProps {
  projectId: () => string | undefined;
  projectReady?: () => boolean;
  /** Currently focused session tab id, for highlight. */
  activeSessionId?: () => string | null;
  /** Open (or focus) a session as a project main-area tab. */
  onOpenSession: (sessionId: string, title?: string | null) => void;
  /** Notify the host to drop any open tab for a session that was deleted here. */
  onSessionDeleted?: (sessionId: string) => void;
}

export function SessionsPanel(props: SessionsPanelProps) {
  const sessions = useSessions(props.projectId);
  const queryClient = useQueryClient();
  const [creating, setCreating] = createSignal(false);
  const [deletingId, setDeletingId] = createSignal<string | null>(null);
  const [actionError, setActionError] = createSignal("");
  const [scrollHost, setScrollHost] = createSignal<HTMLElement | null>(null);

  async function onCreate() {
    const id = props.projectId();
    if (!id || creating()) return;
    if (props.projectReady && !props.projectReady()) return;
    setCreating(true);
    setActionError("");
    try {
      const accepted = await createSession(id, { title: "New session" }, crypto.randomUUID());
      const completed = await waitForOperation(accepted.id);
      const sessionId = completed.target_id ?? accepted.target_id;
      if (!sessionId) throw new Error("Session creation completed without a Session id");
      const session = await getSession(sessionId);
      queryClient.setQueryData(["session", session.id], session);
      queryClient.setQueryData(["session-timeline", session.id], {
        items: [],
        has_older: false,
        has_newer: false,
        oldest_cursor: null,
        newest_cursor: null,
      });
      queryClient.setQueryData<SessionSummary[]>(["sessions", id], (prev) => {
        const list = prev ?? [];
        if (list.some((s) => s.id === session.id)) return list;
        return [session, ...list];
      });
      void sessions.refetch();
      props.onOpenSession(session.id, session.title);
    } catch (error) {
      setActionError(error instanceof Error ? error.message : "Failed to create session");
    } finally {
      setCreating(false);
    }
  }

  async function onDelete(session: SessionSummary) {
    if (deletingId()) return;
    setDeletingId(session.id);
    setActionError("");
    try {
      const accepted = await deleteSession(session.id, session.version, crypto.randomUUID());
      await waitForOperation(accepted.id);
      props.onSessionDeleted?.(session.id);
      void sessions.refetch();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : "Failed to delete session");
    } finally {
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
          title={creating() ? "Creating session — copying workspace…" : "New session"}
          disabled={creating() || (props.projectReady ? !props.projectReady() : false)}
          onClick={() => void onCreate()}
        >
          <Show when={creating()} fallback={<Plus size={14} />}>
            <Loader2 size={14} class="sessions-panel__spin" />
          </Show>
        </button>
      </div>

      <Show when={actionError()}>
        <div class="sessions-panel__error">
          <ErrorBlock message={actionError()} />
        </div>
      </Show>

      <div class="ide-scroll-host sessions-panel__scroll">
        <div class="ide-sidebar-scroll sessions-panel__list" ref={setScrollHost}>
          <Show when={!sessions.isLoading} fallback={<p class="sessions-panel__hint">Loading…</p>}>
            <Show
              when={(sessions.data?.length ?? 0) > 0}
              fallback={
                <EmptyState
                  icon={MessageSquare}
                  title="No sessions"
                  description="Create a Session to chat with the Supervisor on a copy of this project."
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
      classList={{ "sessions-panel__item--active": props.active() }}
      title={props.session.title ?? "Untitled session"}
    >
      <button type="button" class="sessions-panel__select" onClick={props.onOpen}>
        <Alt content={status.label} class="alt-bubble">
          <span class="sessions-panel__status" aria-hidden="true">
            <Show when={status.spinning} fallback={status.icon}>
              <Loader2 size={14} class="sessions-panel__spin" />
            </Show>
          </span>
        </Alt>
        <span class="sessions-panel__title">{props.session.title ?? "Untitled session"}</span>
      </button>
      {/* Trailing row action — invisible until hover/focus, mirroring the legacy
          panel's hover-reveal affordance. */}
      <button
        type="button"
        class="sessions-panel__action"
        title="Delete session"
        aria-label={`Delete session ${props.session.title ?? props.session.id}`}
        disabled={props.deleting()}
        onClick={(event) => {
          event.stopPropagation();
          props.onDelete();
        }}
      >
        <Show when={props.deleting()} fallback={<Trash2 size={14} />}>
          <Loader2 size={14} class="sessions-panel__spin" />
        </Show>
      </button>
    </div>
  );
}

/** Map a server Session state to an icon + label, mirroring the legacy panel. */
function createStatusInfo(state: string): {
  icon: import("solid-js").JSX.Element;
  label: string;
  spinning: boolean;
} {
  switch (state) {
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
