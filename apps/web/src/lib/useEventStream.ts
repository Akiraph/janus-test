import { useQueryClient } from "@tanstack/solid-query";
import { createSignal, onCleanup, onMount } from "solid-js";
import {
  EVENT_CURSOR_RESET,
  getLastEventCursor,
  reconcileEventCursorBounds,
  setLastEventCursor,
} from "./eventCursor";
import { clearRetryStateOnStreamDelta, ingestRetryEvent } from "./modelRetryState";
import { ingestModelStreamEvent } from "./modelStream";

export type ConnectionState = "connecting" | "live" | "reconnecting" | "offline";

interface EventEnvelopeLike {
  event_type?: string;
  resource?: { kind?: string; id?: string } | null;
  payload?: Record<string, unknown> | null;
}

type QueryKey = readonly unknown[];

const INVALIDATION_BATCH_MS = 50;
const INITIAL_EVENT_REPLAY_WINDOW = 32n;

interface EventCursorBounds {
  min: string;
  max: string;
}

export function useEventStream() {
  const queryClient = useQueryClient();
  const [state, setState] = createSignal<ConnectionState>(
    navigator.onLine ? "connecting" : "offline",
  );
  let source: EventSource | undefined;
  let cursorProbe: Promise<void> | undefined;
  let initialCursorProbe: Promise<EventCursorBounds | null> | undefined;
  let primeInitialCursor = true;
  let connectGeneration = 0;
  let invalidationTimer: ReturnType<typeof setTimeout> | undefined;
  const pendingInvalidations = new Map<string, QueryKey>();

  function flushInvalidations() {
    invalidationTimer = undefined;
    const queryKeys = [...pendingInvalidations.values()];
    pendingInvalidations.clear();
    if (queryKeys.length === 0) return;
    void Promise.allSettled(
      queryKeys.map((queryKey) => queryClient.invalidateQueries({ queryKey })),
    );
  }

  function queueInvalidation(queryKey: QueryKey) {
    pendingInvalidations.set(JSON.stringify(queryKey), queryKey);
    if (invalidationTimer === undefined) {
      invalidationTimer = setTimeout(flushInvalidations, INVALIDATION_BATCH_MS);
    }
  }

  function readSystemEventCursor(): Promise<EventCursorBounds | null> {
    if (initialCursorProbe) return initialCursorProbe;
    const probe = fetch("/api/v1/system/info", {
      credentials: "include",
      headers: { Accept: "application/json" },
    })
      .then(async (response) => {
        if (!response.ok) return null;
        const body = (await response.json()) as {
          data?: { events?: { min_cursor?: unknown; max_cursor?: unknown } };
        };
        const min = body.data?.events?.min_cursor;
        const max = body.data?.events?.max_cursor;
        return typeof min === "string" &&
          /^\d+$/.test(min) &&
          typeof max === "string" &&
          /^\d+$/.test(max)
          ? { min, max }
          : null;
      })
      .catch(() => null)
      .finally(() => {
        initialCursorProbe = undefined;
      });
    initialCursorProbe = probe;
    return probe;
  }

  async function cursorForInitialConnection(): Promise<string | null> {
    const persisted = getLastEventCursor();
    if (persisted || !primeInitialCursor) return persisted;
    primeInitialCursor = false;
    const bounds = await readSystemEventCursor();
    if (bounds !== null) {
      // Initial page queries read current state directly. Replay only a small
      // recent window so a page opened during a live turn can recover
      // provisional stream state without invalidating the full event history.
      const max = BigInt(bounds.max);
      const retainedFloor = BigInt(bounds.min) > 0n ? BigInt(bounds.min) - 1n : 0n;
      const replayFrom =
        max > INITIAL_EVENT_REPLAY_WINDOW ? max - INITIAL_EVENT_REPLAY_WINDOW : retainedFloor;
      setLastEventCursor(replayFrom.toString());
      return replayFrom.toString();
    }
    return null;
  }

  function probeCursorBounds() {
    if (cursorProbe) return;
    cursorProbe = fetch("/api/v1/system/info", {
      credentials: "include",
      headers: { Accept: "application/json" },
    })
      .then(async (response) => {
        if (!response.ok) return;
        const body = (await response.json()) as {
          data?: { events?: { min_cursor?: unknown; max_cursor?: unknown } };
        };
        const min = body.data?.events?.min_cursor;
        const max = body.data?.events?.max_cursor;
        const headerMax = response.headers.get("x-janus-event-cursor");
        reconcileEventCursorBounds(
          typeof min === "string" ? min : null,
          headerMax ?? (typeof max === "string" ? max : null),
        );
      })
      .catch(() => {
        // EventSource will continue its normal retry loop when the probe is unavailable.
      })
      .finally(() => {
        cursorProbe = undefined;
      });
  }

  const invalidateForEvent = (envelope: EventEnvelopeLike) => {
    const type = envelope.event_type;
    const resourceId = envelope.resource?.id;
    const resourceKind = envelope.resource?.kind;
    const payload = envelope.payload ?? {};

    if (type === "model.stream_delta") {
      clearRetryStateOnStreamDelta(payload);
      ingestModelStreamEvent(type, payload);
      return;
    }

    if (type === "model.attempt_retrying") {
      ingestRetryEvent(type, payload);
      return;
    }

    if (type === "model_config.changed") {
      queueInvalidation(["model-providers"]);
      return;
    }

    if (type === "project.changed") {
      queueInvalidation(["projects"]);
      if (resourceKind === "project" && resourceId) {
        queueInvalidation(["project", resourceId]);
      }
      if (resourceKind === "github_credential") {
        queueInvalidation(["github-credentials"]);
      }
      return;
    }

    if (type === "project.main_revision_changed") {
      queueInvalidation(["projects"]);
      queueInvalidation(["session-diff"]);
      if (resourceId) {
        queueInvalidation(["project", resourceId]);
        queueInvalidation(["file-tree", resourceId]);
        // Editor saves dirty the working tree but only emit main_revision_changed;
        // keep SCM Changes in sync without requiring a separate git.state_changed.
        queueInvalidation(["git-status", resourceId]);
      }
      return;
    }

    if (type === "git.state_changed") {
      if (resourceId) {
        queueInvalidation(["git-status", resourceId]);
        queueInvalidation(["git-log", resourceId]);
        queueInvalidation(["project", resourceId]);
      }
      queueInvalidation(["projects"]);
      return;
    }

    if (type === "operation.changed") {
      const operationId =
        payload.operation_id ?? (resourceKind === "operation" ? resourceId : undefined);
      if (operationId) {
        queueInvalidation(["operations", operationId]);
      }
      // Clone/delete progress also moves project state.
      queueInvalidation(["projects"]);
      const targetId = payload.target_id ?? payload.project_id;
      if (targetId) {
        queueInvalidation(["project", targetId]);
        queueInvalidation(["git-status", targetId]);
      }
      return;
    }

    if (type === "workspace.diff_changed" || type === "session.revision_changed") {
      const sessionId =
        (resourceKind === "session" ? resourceId : undefined) ??
        (payload as { session_id?: string }).session_id;
      if (sessionId) {
        queueInvalidation(["session", sessionId]);
        queueInvalidation(["session-diff", sessionId]);
        queueInvalidation(["session-timeline", sessionId]);
      }
      const projectId = (payload as { project_id?: string }).project_id;
      if (projectId) {
        queueInvalidation(["projects"]);
        queueInvalidation(["sessions", projectId]);
        queueInvalidation(["project", projectId]);
        queueInvalidation(["file-tree", projectId]);
        queueInvalidation(["git-status", projectId]);
      }
      return;
    }

    if (
      type === "session.changed" ||
      type === "session.deleted" ||
      type === "turn.created" ||
      type === "turn.status_changed" ||
      type === "timeline.item_created" ||
      type === "timeline.item_updated" ||
      type === "model.attempt_retrying" ||
      type === "tool_call.created" ||
      type === "tool_call.changed" ||
      type === "round.changed" ||
      type === "ask.changed" ||
      type === "job.changed" ||
      type === "service.changed" ||
      type === "context.changed" ||
      type === "log.advanced"
    ) {
      const sessionId =
        (resourceKind === "session" ? resourceId : undefined) ??
        (payload as { session_id?: string }).session_id;
      if (sessionId) {
        queueInvalidation(["session", sessionId]);
        queueInvalidation(["session-timeline", sessionId]);
        queueInvalidation(["session-diff", sessionId]);
        queueInvalidation(["turn", sessionId]);
        queueInvalidation(["queued-turns", sessionId]);
        if (type === "context.changed") {
          queueInvalidation(["session-context", sessionId]);
        }
      }
      // Project session lists are keyed by project id when known.
      const projectId = (payload as { project_id?: string }).project_id;
      if (projectId) {
        queueInvalidation(["sessions", projectId]);
      } else {
        queueInvalidation(["sessions"]);
      }
      // Job/Service/Ask resource ids still land in the Session document via timeline.
      if (
        resourceKind === "job" ||
        resourceKind === "service" ||
        resourceKind === "ask" ||
        resourceKind === "tool_call"
      ) {
        queueInvalidation(["session-timeline"]);
      }
      return;
    }

    if (type === "terminal.changed") {
      const terminalId =
        (resourceKind === "terminal" ? resourceId : undefined) ??
        (payload as { terminal_id?: string }).terminal_id;
      if (terminalId) {
        queueInvalidation(["terminal", terminalId]);
      }
      const projectId = (payload as { project_id?: string }).project_id;
      if (projectId) {
        queueInvalidation(["terminals", projectId]);
      } else {
        queueInvalidation(["terminals"]);
      }
    }
  };

  const connect = () => {
    source?.close();
    const generation = ++connectGeneration;
    if (!navigator.onLine) {
      setState("offline");
      return;
    }
    setState(state() === "connecting" ? "connecting" : "reconnecting");
    // Resume from the last durable cursor so a reconnect (incl. after page
    // refresh) does not replay the entire retained history. EventSource also
    // auto-sends `Last-Event-ID` from the browser; the backend requires it to
    // match this `after` value (CURSOR_MISMATCH otherwise), so the cursor we
    // persisted must be the same one the browser remembers.
    void (async () => {
      const cursor = await cursorForInitialConnection();
      if (generation !== connectGeneration || !navigator.onLine) return;
      const url = cursor ? `/api/v1/events?after=${encodeURIComponent(cursor)}` : "/api/v1/events";
      const nextSource = new EventSource(url);
      source = nextSource;
      nextSource.addEventListener("open", () => setState("live"));
      nextSource.addEventListener("janus", (event) => {
        try {
          const data = JSON.parse((event as MessageEvent).data) as EventEnvelopeLike;
          // Advance the durable cursor from the SSE `id:` frame so the next
          // reconnect resumes after this event.
          const id = (event as MessageEvent).lastEventId;
          if (id) setLastEventCursor(id);
          invalidateForEvent(data);
        } catch {
          // Keep the stream resilient; a single malformed frame must not break UI.
        }
      });
      nextSource.addEventListener("error", () => {
        setState(navigator.onLine ? "reconnecting" : "offline");
        probeCursorBounds();
      });
    })();
  };

  const handleOnline = () => connect();
  const handleCursorReset = () => connect();
  const handleOffline = () => {
    source?.close();
    setState("offline");
  };

  onMount(() => {
    window.addEventListener("online", handleOnline);
    window.addEventListener("offline", handleOffline);
    window.addEventListener(EVENT_CURSOR_RESET, handleCursorReset);
    connect();
  });
  onCleanup(() => {
    connectGeneration += 1;
    source?.close();
    if (invalidationTimer !== undefined) clearTimeout(invalidationTimer);
    pendingInvalidations.clear();
    window.removeEventListener("online", handleOnline);
    window.removeEventListener("offline", handleOffline);
    window.removeEventListener(EVENT_CURSOR_RESET, handleCursorReset);
  });

  return state;
}
