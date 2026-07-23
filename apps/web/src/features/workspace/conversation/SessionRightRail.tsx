import { ChevronLeft, ChevronRight } from "lucide-react";
import { type ReactNode, useEffect, useRef } from "react";
import { cn } from "../../../lib/cn";
import { useResizableRail } from "../hooks/useResizableRail";

export interface SessionRailSection {
  readonly id: string;
  readonly title: string;
  readonly content: ReactNode;
}

export interface SessionRightRailProps {
  readonly sections: readonly SessionRailSection[];
  readonly contentKey: string;
  readonly focusKey?: string | undefined;
}

export function SessionRightRail({
  sections,
  contentKey,
  focusKey,
}: SessionRightRailProps) {
  const {
    width,
    collapsed,
    isDragging,
    expand,
    onResizeStart,
    toggleCollapsed,
  } = useResizableRail();
  const previousContentKeyRef = useRef(contentKey);
  const previousFocusKeyRef = useRef(focusKey);

  useEffect(() => {
    if (sections.length === 0) {
      previousContentKeyRef.current = contentKey;
      return;
    }

    if (previousContentKeyRef.current !== contentKey) {
      previousContentKeyRef.current = contentKey;
      expand();
    }
  }, [contentKey, expand, sections.length]);

  useEffect(() => {
    if (focusKey === undefined || previousFocusKeyRef.current === focusKey) {
      return;
    }

    previousFocusKeyRef.current = focusKey;
    expand();
  }, [expand, focusKey]);

  if (sections.length === 0) {
    return null;
  }

  return (
    <div
      className={cn(
        "relative flex shrink-0 flex-col border-l border-border bg-background",
        // Animate collapse/expand — but NOT mid-drag, or the resize handle eases
        // behind the pointer instead of tracking it.
        isDragging ? "" : "transition-[width] duration-200 ease-out",
      )}
      style={{ width: collapsed ? 1 : width }}
    >
      {/* Toggle lives just outside the left edge (right-full); the container is
          never clipped, so it stays visible in both states. */}
      <RailToggle collapsed={collapsed} onClick={toggleCollapsed} />

      {/* Drag-to-resize handle — only meaningful while expanded. */}
      {!collapsed && (
        <div
          onPointerDown={onResizeStart}
          className="absolute -left-1 top-0 z-10 h-full w-2 cursor-col-resize"
          aria-hidden
        />
      )}

      {/* Content clips as the rail collapses; the inner column keeps its full
          width so text slides out cleanly instead of reflowing/squishing. */}
      <div className="min-h-0 flex-1 overflow-hidden">
        <div
          className={cn(
            "h-full space-y-3 overflow-y-auto p-3",
            collapsed && "pointer-events-none",
          )}
          style={{ width }}
          aria-hidden={collapsed}
        >
          {sections.map((section) => (
            <RailSection key={section.id} title={section.title}>
              {section.content}
            </RailSection>
          ))}
        </div>
      </div>
    </div>
  );
}

function RailToggle({
  collapsed,
  onClick,
}: {
  readonly collapsed: boolean;
  readonly onClick: () => void;
}) {
  const Icon = collapsed ? ChevronLeft : ChevronRight;
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={collapsed ? "Show session panel" : "Hide session panel"}
      className="absolute right-full top-1/2 z-20 flex h-8 w-7 -translate-y-1/2 items-center justify-center rounded-l-sm rounded-r-none border border-r-0 border-border bg-background text-faint shadow-card transition-colors hover:bg-muted hover:text-foreground"
    >
      <Icon className="h-3.5 w-3.5" aria-hidden />
    </button>
  );
}

function RailSection({
  title,
  children,
}: {
  readonly title: string;
  readonly children: ReactNode;
}) {
  return (
    <section className="min-h-0 space-y-2">
      <div className="flex items-center gap-1 text-2xs font-semibold text-faint">
        <ChevronRight className="h-3 w-3" aria-hidden />
        {title}
      </div>
      <div className="min-h-0">{children}</div>
    </section>
  );
}

/** Shared empty-state for a rail section with nothing to show yet. */
export function RailEmptyState({ children }: { readonly children: ReactNode }) {
  return (
    <div
      className={cn(
        "flex min-h-16 items-center justify-center rounded-md border border-dashed border-border bg-background/50 px-3 py-4 text-center text-2xs text-faint",
      )}
    >
      {children}
    </div>
  );
}
