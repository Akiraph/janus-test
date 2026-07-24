import CalendarDays from "lucide-solid/icons/calendar-days";
import GitMerge from "lucide-solid/icons/git-merge";
import UserRound from "lucide-solid/icons/user-round";
import { Show } from "solid-js";
import type { GitLogEntryView } from "../../../lib/api";

/** Format an ISO commit time as a localized medium date + short time. */
export function formatCommitTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(
    date,
  );
}

/**
 * Structured commit summary used inside the custom Alt tooltip. Tolerates
 * partial payloads (older servers may omit date/stats) so the bubble never
 * renders empty slots as `undefined`.
 */
export function CommitAltContent(props: { commit: GitLogEntryView }) {
  const parents =
    (props.commit.parents?.length ?? 0) === 0
      ? "root"
      : props.commit.parents.map((p) => p.slice(0, 7)).join(", ");
  const insertions = props.commit.insertions ?? 0;
  const deletions = props.commit.deletions ?? 0;
  const changed = props.commit.changed_files ?? 0;
  return (
    <div class="alt-bubble--commit">
      <span class="alt-commit-message">{props.commit.message}</span>
      <span class="alt-commit-row">
        <UserRound size={12} aria-hidden="true" />
        <span>{props.commit.author}</span>
      </span>
      <span class="alt-commit-row">
        <CalendarDays size={12} aria-hidden="true" />
        <span class="alt-commit-time">{formatCommitTime(props.commit.committed_at)}</span>
      </span>
      <span class="alt-commit-row">
        <GitMerge size={12} aria-hidden="true" />
        <code>{props.commit.sha.slice(0, 7)}</code>
        <span>· parents: {parents}</span>
      </span>
      <span class="alt-commit-stats">
        <span class="add">+{insertions}</span>
        <span class="del">-{deletions}</span>
        <span>{changed} files</span>
      </span>
      <Show when={!props.commit.committed_at}>
        <span class="alt-commit-missing">Date unavailable from server</span>
      </Show>
    </div>
  );
}
