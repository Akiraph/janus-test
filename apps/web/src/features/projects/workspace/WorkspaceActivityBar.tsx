import Files from "lucide-solid/icons/files";
import GitCompare from "lucide-solid/icons/git-compare";
import MessageSquare from "lucide-solid/icons/message-square";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import type { JSX } from "solid-js";
import type { WorkspaceActivity } from "./workspaceState";

interface WorkspaceActivityBarProps {
  activity: WorkspaceActivity;
  navigationOpen: boolean;
  ready: boolean;
  onSelect: (activity: WorkspaceActivity) => void;
}

const ACTIVITIES: ReadonlyArray<{
  value: WorkspaceActivity;
  label: string;
  icon: (props: { size: number }) => JSX.Element;
}> = [
  { value: "sessions", label: "Sessions", icon: MessageSquare },
  { value: "explorer", label: "Explorer", icon: Files },
  { value: "scm", label: "Source Control", icon: GitCompare },
  { value: "terminal", label: "Terminal", icon: TerminalSquare },
];

export function WorkspaceActivityBar(props: WorkspaceActivityBarProps) {
  return (
    <nav class="ide-activity-bar" aria-label="Workspace activity">
      {ACTIVITIES.map((item) => {
        const Icon = item.icon;
        return (
          <button
            type="button"
            class="ide-activity-btn"
            classList={{
              "ide-activity-btn--active": props.activity === item.value && props.navigationOpen,
            }}
            aria-label={item.label}
            aria-pressed={props.activity === item.value && props.navigationOpen}
            title={item.label}
            disabled={!props.ready}
            onClick={() => props.onSelect(item.value)}
          >
            <Icon size={18} />
            <span>{item.label}</span>
          </button>
        );
      })}
    </nav>
  );
}
