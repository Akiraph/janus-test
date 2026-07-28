import { useQueryClient } from "@tanstack/solid-query";
import { createSignal, onCleanup, onMount } from "solid-js";

export type ConnectionState = "connecting" | "live" | "reconnecting" | "offline";

interface EventEnvelopeLike {
  event_type?: string;
  resource?: { kind?: string; id?: string } | null;
  payload?: {
    operation_id?: string;
    project_id?: string;
    target_id?: string;
  } | null;
}

export function useEventStream() {
  const queryClient = useQueryClient();
  const [state, setState] = createSignal<ConnectionState>(
    navigator.onLine ? "connecting" : "offline",
  );
  let source: EventSource | undefined;

  const invalidateForEvent = (envelope: EventEnvelopeLike) => {
    const type = envelope.event_type;
    const resourceId = envelope.resource?.id;
    const resourceKind = envelope.resource?.kind;
    const payload = envelope.payload ?? {};

    if (type === "model_config.changed") {
      void queryClient.invalidateQueries({ queryKey: ["model-providers"] });
      return;
    }

    if (type === "project.changed") {
      void queryClient.invalidateQueries({ queryKey: ["projects"] });
      if (resourceKind === "project" && resourceId) {
        void queryClient.invalidateQueries({ queryKey: ["project", resourceId] });
      }
      if (resourceKind === "github_credential") {
        void queryClient.invalidateQueries({ queryKey: ["github-credentials"] });
      }
      return;
    }

    if (type === "project.main_revision_changed") {
      void queryClient.invalidateQueries({ queryKey: ["projects"] });
      if (resourceId) {
        void queryClient.invalidateQueries({ queryKey: ["project", resourceId] });
        void queryClient.invalidateQueries({ queryKey: ["file-tree", resourceId] });
        // Editor saves dirty the working tree but only emit main_revision_changed;
        // keep SCM Changes in sync without requiring a separate git.state_changed.
        void queryClient.invalidateQueries({ queryKey: ["git-status", resourceId] });
      }
      return;
    }

    if (type === "git.state_changed") {
      if (resourceId) {
        void queryClient.invalidateQueries({ queryKey: ["git-status", resourceId] });
        void queryClient.invalidateQueries({ queryKey: ["git-log", resourceId] });
        void queryClient.invalidateQueries({ queryKey: ["project", resourceId] });
      }
      void queryClient.invalidateQueries({ queryKey: ["projects"] });
      return;
    }

    if (type === "operation.changed") {
      const operationId =
        payload.operation_id ?? (resourceKind === "operation" ? resourceId : undefined);
      if (operationId) {
        void queryClient.invalidateQueries({ queryKey: ["operations", operationId] });
      }
      // Clone/delete progress also moves project state.
      void queryClient.invalidateQueries({ queryKey: ["projects"] });
      const targetId = payload.target_id ?? payload.project_id;
      if (targetId) {
        void queryClient.invalidateQueries({ queryKey: ["project", targetId] });
        void queryClient.invalidateQueries({ queryKey: ["git-status", targetId] });
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
      type === "model.stream_delta" ||
      type === "model.attempt_changed" ||
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
        void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["session-timeline", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["session-diff", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["turn", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["terminals", "session", sessionId] });
      }
      // Project session lists are keyed by project id when known.
      const projectId = (payload as { project_id?: string }).project_id;
      if (projectId) {
        void queryClient.invalidateQueries({ queryKey: ["sessions", projectId] });
      } else {
        void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      }
      // Job/Service/Ask resource ids still land in the Session document via timeline.
      if (
        resourceKind === "job" ||
        resourceKind === "service" ||
        resourceKind === "ask" ||
        resourceKind === "tool_call"
      ) {
        void queryClient.invalidateQueries({ queryKey: ["session-timeline"] });
      }
      return;
    }

    if (type === "terminal.changed") {
      const terminalId =
        (resourceKind === "terminal" ? resourceId : undefined) ??
        (payload as { terminal_id?: string }).terminal_id;
      if (terminalId) {
        void queryClient.invalidateQueries({ queryKey: ["terminal", terminalId] });
      }
      const ownerKind = (payload as { owner_kind?: string }).owner_kind;
      const ownerId =
        (payload as { owner_id?: string; project_id?: string; session_id?: string }).owner_id ??
        (payload as { project_id?: string }).project_id ??
        (payload as { session_id?: string }).session_id;
      if (ownerKind && ownerId) {
        void queryClient.invalidateQueries({ queryKey: ["terminals", ownerKind, ownerId] });
      } else {
        void queryClient.invalidateQueries({ queryKey: ["terminals"] });
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
    source = new EventSource("/api/v1/events");
    source.addEventListener("open", () => setState("live"));
    source.addEventListener("janus", (event) => {
      void queryClient.invalidateQueries({ queryKey: ["system-info"] });
      try {
        const data = JSON.parse((event as MessageEvent).data) as EventEnvelopeLike;
        invalidateForEvent(data);
      } catch {
        // Keep the stream resilient; a single malformed frame must not break UI.
      }
    });
    source.addEventListener("error", () => {
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
    connect();
  });
  onCleanup(() => {
    source?.close();
    window.removeEventListener("online", handleOnline);
    window.removeEventListener("offline", handleOffline);
  });

  return state;
}
