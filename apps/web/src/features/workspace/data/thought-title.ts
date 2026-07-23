import type { ThoughtConversationItem } from "../types";

export function formatCompletedThoughtTitle(
  item: ThoughtConversationItem,
): string {
  const startedAt = Date.parse(item.startedAt ?? item.at);
  const completedAt = Date.parse(item.completedAt ?? item.at);

  if (!Number.isFinite(startedAt) || !Number.isFinite(completedAt)) {
    return item.title;
  }

  const durationMilliseconds = Math.max(0, completedAt - startedAt);
  return durationMilliseconds < 1000
    ? "Thought for a while"
    : `Thought for ${formatDuration(durationMilliseconds)}`;
}

export function formatDuration(milliseconds: number): string {
  const seconds = Math.max(0, Math.round(milliseconds / 1000));

  if (seconds < 60) {
    return `${seconds}s`;
  }

  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  return remainingSeconds === 0
    ? `${minutes}m`
    : `${minutes}m ${remainingSeconds}s`;
}
