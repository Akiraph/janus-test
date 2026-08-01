/**
 * Cross-tab session list synchronization via BroadcastChannel.
 *
 * When a session is created or deleted in one tab, other tabs invalidate
 * their session list queries so the sidebar stays consistent without
 * requiring manual refresh or waiting for the next poll cycle.
 */

const CHANNEL_NAME = "janus:session-list";
const sourceId = crypto.randomUUID();

let channel: BroadcastChannel | null = null;

function getChannel(): BroadcastChannel {
  if (!channel) {
    try {
      channel = new BroadcastChannel(CHANNEL_NAME);
    } catch {
      // BroadcastChannel unavailable (very old browser) — degrade silently.
      return null as unknown as BroadcastChannel;
    }
  }
  return channel;
}

export function broadcastSessionListChanged(projectId: string): void {
  const ch = getChannel();
  if (!ch) return;
  try {
    ch.postMessage({ type: "session-list-changed", projectId, sourceId });
  } catch {
    // Best-effort.
  }
}

export function subscribeSessionListChanged(projectId: string, callback: () => void): () => void {
  const ch = getChannel();
  if (!ch) return () => {};

  const handler = (event: MessageEvent) => {
    try {
      const data = event.data as {
        type?: string;
        projectId?: string;
        sourceId?: string;
      };
      if (
        data.type === "session-list-changed" &&
        data.projectId === projectId &&
        data.sourceId !== sourceId
      ) {
        callback();
      }
    } catch {
      // Malformed broadcast — ignore.
    }
  };

  ch.addEventListener("message", handler);
  return () => ch.removeEventListener("message", handler);
}
