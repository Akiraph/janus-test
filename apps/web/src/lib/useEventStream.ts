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
