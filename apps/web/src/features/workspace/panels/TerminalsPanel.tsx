import { Plus, TerminalSquare } from "lucide-react";
import { Button } from "../../../components/ui/button";
import { EmptyState } from "../../../components/ui/empty-state";

interface TerminalEntry {
  readonly key: string;
  readonly label: string;
}

interface TerminalsPanelProps {
  readonly terminals: ReadonlyArray<TerminalEntry>;
  readonly activeKey: string | null;
  readonly onSelect: (key: string) => void;
  readonly onNew: () => void;
}

/**
 * TerminalsPanel — sidebar list of open terminals. Mirrors SessionsPanel:
 * selecting the Terminal category in the rail shows this list, and opening a
 * terminal from here is what creates a terminal tab in the editor area.
 */
export function TerminalsPanel({
  terminals,
  activeKey,
  onSelect,
  onNew,
}: TerminalsPanelProps) {
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-11 shrink-0 items-center justify-between gap-2 border-b border-border px-4">
        <div className="flex min-w-0 items-center gap-2">
          <TerminalSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
          <h2 className="truncate text-sm font-semibold">Terminals</h2>
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={onNew}
          className="h-7 gap-1.5 px-2 text-xs"
        >
          <Plus className="h-3.5 w-3.5" />
          New terminal
        </Button>
      </div>

      {terminals.length === 0 ? (
        <div className="flex flex-1 items-center justify-center p-6">
          <EmptyState
            icon={<TerminalSquare className="h-12 w-12" />}
            title="No terminals"
            description="Open a terminal to run commands in this project."
            action={
              <Button onClick={onNew} className="gap-2">
                <Plus className="h-4 w-4" />
                New terminal
              </Button>
            }
          />
        </div>
      ) : (
        <div className="flex-1 overflow-y-auto p-2">
          <div className="space-y-1">
            {terminals.map((term) => (
              <button
                key={term.key}
                type="button"
                onClick={() => onSelect(term.key)}
                className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors hover:bg-muted ${
                  term.key === activeKey ? "bg-info-soft text-foreground" : ""
                }`}
              >
                <TerminalSquare className="h-4 w-4 shrink-0" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">
                    {term.label}
                  </span>
                </span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
