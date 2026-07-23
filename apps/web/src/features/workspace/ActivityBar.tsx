import type { LucideIcon } from "lucide-react";
import { Tooltip } from "../../components/ui/tooltip";
import { cn } from "../../lib/cn";

export interface ActivityItem {
  readonly id: string;
  readonly label: string;
  readonly icon: LucideIcon;
}

export interface ActivityBarProps {
  readonly items: ReadonlyArray<ActivityItem>;
  /** Currently highlighted item, or null when nothing is active. */
  readonly activeId: string | null;
  readonly onSelect: (id: string) => void;
}

/**
 * ActivityBar — VSCode-style vertical icon rail on the far left.
 *
 * Holds the categories (Sessions / Files / Git / Terminal). Selecting one
 * either switches what the side bar lists or opens a document tab — the parent
 * decides per id. Categories live here; only the concrete session/file you
 * open from a list becomes an editor tab.
 */
export function ActivityBar({ items, activeId, onSelect }: ActivityBarProps) {
  return (
    <nav className="flex w-16 shrink-0 flex-col items-stretch border-r border-border bg-background">
      {items.map(({ id, label, icon: Icon }) => {
        const isActive = id === activeId;
        return (
          <Tooltip key={id} content={label} side="right">
            <button
              type="button"
              aria-label={label}
              aria-current={isActive}
              onClick={() => onSelect(id)}
              className={cn(
                "relative flex w-full flex-col items-center gap-1 py-2.5 text-[10px] font-medium transition-colors",
                isActive
                  ? "bg-info-soft text-foreground"
                  : "text-muted-foreground hover:bg-muted hover:text-foreground",
              )}
            >
              {isActive && (
                <span className="absolute left-0 top-0 bottom-0 w-0.5 bg-border-accent" />
              )}
              <Icon className="h-5 w-5" />
              <span>{label}</span>
            </button>
          </Tooltip>
        );
      })}
    </nav>
  );
}
