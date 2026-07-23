import { CheckCircle2, Circle, Loader2 } from "lucide-react";
import { cn } from "../../../lib/cn";
import type { TodoItemView } from "../types";
import { RailEmptyState } from "./SessionRightRail";

interface TodoSectionProps {
  /** Latest todo snapshot for the session; undefined when no run has set one. */
  readonly items: readonly TodoItemView[] | undefined;
}

/**
 * TodoSection — the session's current todo list, rendered in the right rail.
 * Replaces the old inline TodoOutput bubble that sat inside the chat stream.
 * The list is the most recent todo snapshot (see latestTodoFromRuns); items are
 * read-only — status is driven by the supervisor, not toggled here.
 */
export function TodoSection({ items }: TodoSectionProps) {
  if (items === undefined || items.length === 0) {
    return <RailEmptyState>No todo list yet</RailEmptyState>;
  }

  return (
    <ul className="flex flex-col gap-0.5">
      {items.map((item) => (
        <TodoRow key={item.id} item={item} />
      ))}
    </ul>
  );
}

function TodoRow({ item }: { readonly item: TodoItemView }) {
  const { Icon, iconClass } = todoIcon(item.status);
  return (
    <li
      className={cn(
        "flex min-w-0 items-start gap-2 rounded-sm px-1.5 py-1 text-2xs leading-relaxed transition-colors",
        item.status === "in_progress"
          ? "bg-info-soft/60 text-foreground"
          : "text-muted-foreground",
      )}
    >
      <Icon
        className={cn("mt-0.5 h-3.5 w-3.5 shrink-0", iconClass)}
        aria-hidden
      />
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-words">
        {item.content}
      </span>
    </li>
  );
}

function todoIcon(status: TodoItemView["status"]): {
  readonly Icon: typeof Circle;
  readonly iconClass: string;
} {
  switch (status) {
    case "completed":
      return { Icon: CheckCircle2, iconClass: "text-success" };
    case "in_progress":
      return { Icon: Loader2, iconClass: "text-info animate-spin" };
    case "pending":
      return { Icon: Circle, iconClass: "text-faint" };
  }
}
