import { useQueryClient } from "@tanstack/solid-query";
import { createSignal, onCleanup, onMount } from "solid-js";
import { initStreamTextListener } from "./modelStream";

export type ConnectionState = "connecting" | "live" | "reconnecting" | "offline";

interface StateFrame {
  kind: string;
  id?: string | null;
  data: unknown;
  cursor?: string | null;
}

/** Return whether a frame belongs at or after the last applied event cursor. */
export function shouldApplyCursor(
  lastCursor: number | null,
  rawCursor: string | null | undefined,
): boolean {
  if (rawCursor == null || rawCursor === "") return true;
  const cursor = Number(rawCursor);
  if (!Number.isSafeInteger(cursor) || cursor < 0) return false;
  return lastCursor === null || cursor >= lastCursor;
}

function numericCursor(rawCursor: string | null | undefined): number | null {
  if (rawCursor == null || rawCursor === "") return null;
  const cursor = Number(rawCursor);
  return Number.isSafeInteger(cursor) && cursor >= 0 ? cursor : null;
}

export function useEventStream() {
  const queryClient = useQueryClient();
  const [state, setState] = createSignal<ConnectionState>(
    navigator.onLine ? "connecting" : "offline",
  );
  let source: EventSource | undefined;
  let lastCursor: number | null = null;

  const handleState = (frame: StateFrame, eventCursor?: string | null) => {
    const cursorValue = frame.cursor ?? eventCursor;
    if (!shouldApplyCursor(lastCursor, cursorValue)) return;
    const cursor = numericCursor(cursorValue);
    if (cursor !== null) lastCursor = Math.max(lastCursor ?? cursor, cursor);
    const { kind, id, data } = frame;

    switch (kind) {
      case "session":
        queryClient.setQueryData(["session", id], data);
        break;
      case "session_timeline":
        queryClient.setQueryData(["session-timeline", id], data);
        break;
      case "session_context":
        queryClient.setQueryData(["session-context", id], data);
        break;
      case "turn":
        if (id) {
          const separator = id.indexOf("_");
          if (separator > 0 && separator < id.length - 1) {
            queryClient.setQueryData(
              ["turn", id.slice(0, separator), id.slice(separator + 1)],
              data,
            );
          }
        }
        break;
      case "queued_turns":
        queryClient.setQueryData(["queued-turns", id], data);
        break;
      case "async_tasks":
        queryClient.setQueryData(["async-tasks"], data);
        break;
      case "terminal":
        queryClient.setQueryData(["terminal", id], data);
        break;
      case "terminals":
        queryClient.setQueryData(["terminals", id], data);
        break;
      case "notification_channels":
        queryClient.setQueryData(["notification-channels"], data);
        break;
      case "project":
        queryClient.setQueryData(["project", id], data);
        break;
      case "projects":
        queryClient.setQueryData(["projects"], data);
        break;
      case "operation":
        queryClient.setQueryData(["operations", id], data);
        queryClient.invalidateQueries({ queryKey: ["automations"] });
        break;
      case "git_status":
        queryClient.setQueryData(["git-status", id], data);
        break;
      case "git_log":
        queryClient.setQueryData(["git-log", id, 50], data);
        break;
      case "sessions":
        queryClient.setQueryData(["sessions", id], data);
        break;
      case "providers":
        queryClient.setQueryData(["model-providers"], data);
        break;
      case "system_info":
        queryClient.setQueryData(["system-info"], data);
        break;
      case "bootstrap":
        queryClient.setQueryData(["bootstrap"], data);
        break;
      case "stream_text":
        window.dispatchEvent(
          new CustomEvent("janus:stream-text", {
            detail: { id, data },
          }),
        );
        break;
    }
  };

  // Apply the server snapshot. Each field maps to the same query key the GET
  // endpoints use, so a connect/reconnect converges without invalidating
  // anything (invalidation would refetch and flash stale UI).
  const handleSnapshot = (frame: Record<string, unknown>, eventCursor?: string | null) => {
    if (!shouldApplyCursor(lastCursor, eventCursor)) return;
    const cursor = numericCursor(eventCursor);
    if (cursor !== null) lastCursor = Math.max(lastCursor ?? cursor, cursor);
    for (const [kind, data] of Object.entries(frame)) {
      if (data === null || data === undefined) continue;
      switch (kind) {
        case "bootstrap":
          queryClient.setQueryData(["bootstrap"], data);
          break;
        case "system_info":
          queryClient.setQueryData(["system-info"], data);
          break;
        case "projects":
          queryClient.setQueryData(["projects"], data);
          break;
        case "providers":
          queryClient.setQueryData(["model-providers"], data);
          break;
      }
    }
  };

  const connect = () => {
    source?.close();
    if (!navigator.onLine) {
      setState("offline");
      return;
    }
    setState(state() === "connecting" ? "connecting" : "reconnecting");

    const nextSource = new EventSource("/api/v1/events");
    source = nextSource;

    nextSource.addEventListener("open", () => {
      setState("live");
      // No invalidateQueries here: the server replays missed projections from
      // the browser's Last-Event-ID and sends a fresh snapshot, which converge
      // the cache without refetching.
    });

    nextSource.addEventListener("state", (event) => {
      try {
        const message = event as MessageEvent;
        const frame = JSON.parse(message.data) as StateFrame;
        handleState(frame, message.lastEventId || null);
      } catch {
        // Malformed frame — ignore.
      }
    });

    nextSource.addEventListener("snapshot", (event) => {
      try {
        const message = event as MessageEvent;
        const frame = JSON.parse(message.data) as Record<string, unknown>;
        handleSnapshot(frame, message.lastEventId || null);
      } catch {
        // Malformed snapshot — ignore.
      }
    });

    nextSource.addEventListener("error", () => {
      setState(navigator.onLine ? "reconnecting" : "offline");
    });
  };

  const handleOnline = () => connect();
  const handleOffline = () => {
    source?.close();
    setState("offline");
  };

  onMount(() => {
    window.addEventListener("online", handleOnline);
    window.addEventListener("offline", handleOffline);
    const cleanupStreamText = initStreamTextListener();
    connect();
    onCleanup(() => {
      cleanupStreamText?.();
      source?.close();
      window.removeEventListener("online", handleOnline);
      window.removeEventListener("offline", handleOffline);
    });
  });

  return state;
}
