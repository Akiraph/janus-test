const channelName = "janus.project-threads";
const sourceId =
  globalThis.crypto?.randomUUID?.() ??
  `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`;

let channel: BroadcastChannel | undefined;

export function broadcastProjectThreadsChanged(projectId: string): void {
  getProjectThreadsChannel()?.postMessage({
    type: "project_threads_changed",
    projectId,
    sourceId,
  });
}

export function subscribeProjectThreadsBroadcast(
  projectId: string,
  onChanged: () => void,
): () => void {
  const currentChannel = getProjectThreadsChannel();

  if (currentChannel === undefined) {
    return () => undefined;
  }

  const handleMessage = (event: MessageEvent<unknown>) => {
    if (!isProjectThreadsBroadcastMessage(event.data)) {
      return;
    }

    if (
      event.data.sourceId === sourceId ||
      event.data.projectId !== projectId
    ) {
      return;
    }

    onChanged();
  };

  currentChannel.addEventListener("message", handleMessage);

  return () => {
    currentChannel.removeEventListener("message", handleMessage);
  };
}

function getProjectThreadsChannel(): BroadcastChannel | undefined {
  if (typeof BroadcastChannel === "undefined") {
    return undefined;
  }

  channel ??= new BroadcastChannel(channelName);
  return channel;
}

function isProjectThreadsBroadcastMessage(value: unknown): value is {
  readonly type: "project_threads_changed";
  readonly projectId: string;
  readonly sourceId: string;
} {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    value.type === "project_threads_changed" &&
    "projectId" in value &&
    typeof value.projectId === "string" &&
    "sourceId" in value &&
    typeof value.sourceId === "string"
  );
}
