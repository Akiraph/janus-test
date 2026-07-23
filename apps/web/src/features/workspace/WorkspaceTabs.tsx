import type { LucideIcon } from "lucide-react";
import { X } from "lucide-react";
import { cn } from "../../lib/cn";

export interface WorkspaceTab {
  readonly id: string;
  readonly label: string;
  readonly icon: LucideIcon;
  /** True when the tab holds unsaved edits (file tabs only). */
  readonly dirty?: boolean;
}

export interface WorkspaceTabsProps {
  readonly tabs: ReadonlyArray<WorkspaceTab>;
  readonly activeId: string | null;
  readonly onSelect: (id: string) => void;
  readonly onClose: (id: string) => void;
}

/**
 * WorkspaceTabs — browser-style tab strip for the open workspace views.
 * Tabs are opened from the ActivityBar, switched by click, and closed via
 * the ✕ affordance. A tab with unsaved edits shows a solid dot instead of
 * the ✕ until hovered (mirrors VS Code).
 */
export function WorkspaceTabs({
  tabs,
  activeId,
  onSelect,
  onClose,
}: WorkspaceTabsProps) {
  return (
    <div className="flex h-10 shrink-0 items-stretch gap-1 border-b border-border bg-background px-2 pt-1.5">
      {tabs.map(({ id, label, icon: Icon, dirty }) => {
        const isActive = id === activeId;
        return (
          <div
            key={id}
            className={cn(
              "group flex items-center gap-2 rounded-t-md border border-b-0 px-3 text-xs transition-colors",
              isActive
                ? "border-border bg-card text-foreground"
                : "border-transparent text-muted-foreground hover:bg-muted",
            )}
          >
            <button
              type="button"
              onClick={() => onSelect(id)}
              className="flex items-center gap-1.5 py-2"
            >
              <Icon className="h-3.5 w-3.5" />
              <span className="font-medium">{label}</span>
            </button>
            <button
              type="button"
              aria-label={`Close ${label}`}
              onClick={() => onClose(id)}
              className={cn(
                "relative flex h-4 w-4 items-center justify-center rounded-sm transition-colors hover:bg-border-strong/60",
                isActive ? "opacity-70" : "opacity-0 group-hover:opacity-70",
              )}
            >
              {/* Dirty tabs show a dot until hover reveals the close X. */}
              {dirty ? (
                <span
                  className="block h-2.5 w-2.5 rounded-full bg-foreground/70 transition-opacity group-hover:opacity-0"
                  aria-hidden
                />
              ) : null}
              <X
                className={cn(
                  "h-3 w-3 transition-opacity",
                  dirty
                    ? "absolute opacity-0 group-hover:opacity-100"
                    : "opacity-100",
                )}
              />
            </button>
          </div>
        );
      })}
    </div>
  );
}
