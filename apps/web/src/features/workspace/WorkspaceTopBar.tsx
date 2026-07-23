import { LogOut, Menu, Settings, SlidersHorizontal } from "lucide-react";
import { Button } from "../../components/ui/button";
import { DropdownMenu } from "../../components/ui/dropdown-menu";

export interface WorkspaceTopBarProps {
  readonly projectName: string;
  readonly onOpenWorkspaceSettings: () => void;
  readonly onOpenSettings: () => void;
  readonly onExit: () => void;
}

/**
 * WorkspaceTopBar — the single top bar for a project workspace.
 *
 * Layout:
 * - Left:  hamburger menu (Workspace Settings / Exit) + "Workspace: {name}"
 * - Right: global app settings
 *
 * The model selector now lives inside the conversation composer, not here.
 */
export function WorkspaceTopBar({
  projectName,
  onOpenWorkspaceSettings,
  onOpenSettings,
  onExit,
}: WorkspaceTopBarProps) {
  return (
    <header className="flex h-14 shrink-0 items-center justify-between gap-2 border-b border-border px-3">
      {/* Left: hamburger + workspace name */}
      <div className="flex min-w-0 items-center gap-2">
        <DropdownMenu
          align="start"
          trigger={
            <button
              type="button"
              aria-label="Workspace menu"
              className="flex h-9 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-border-accent/60"
            >
              <Menu className="h-5 w-5" />
            </button>
          }
          items={[
            {
              id: "workspace-settings",
              label: "Workspace Settings",
              icon: <SlidersHorizontal className="h-4 w-4" />,
              onClick: onOpenWorkspaceSettings,
            },
            "separator",
            {
              id: "exit",
              label: "Exit",
              icon: <LogOut className="h-4 w-4" />,
              onClick: onExit,
            },
          ]}
        />
        <div className="min-w-0 truncate text-sm">
          <span className="text-muted-foreground">Workspace: </span>
          <span className="font-semibold text-foreground">{projectName}</span>
        </div>
      </div>

      {/* Right: global settings */}
      <Button variant="ghost" size="sm" onClick={onOpenSettings}>
        <Settings className="h-4 w-4" />
        <span>Settings</span>
      </Button>
    </header>
  );
}
