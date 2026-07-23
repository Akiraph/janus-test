import { ArrowLeft, KeyRound, Server, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { cn } from "../../lib/cn";
import { GitHubCredentialsPanel } from "./GitHubCredentialsPanel";
import { ProvidersPanel } from "./ProvidersPanel";
import { SecurityPanel } from "./SecurityPanel";

type SettingsSection = "providers" | "credentials" | "security";

const SECTIONS: ReadonlyArray<{
  readonly id: SettingsSection;
  readonly label: string;
  readonly icon: typeof Server;
}> = [
  { id: "providers", label: "Model Providers", icon: Server },
  { id: "credentials", label: "Credentials", icon: KeyRound },
  { id: "security", label: "Security", icon: ShieldCheck },
];

/**
 * SettingsPage — dedicated `/settings` route.
 *
 * No top header bar: navigation (title + back + categories) lives entirely in
 * the left rail. The selected category renders on the right. Being a route
 * (not a modal) means the provider/credential forms open as dialogs without
 * nesting dialog-in-dialog.
 */
export function SettingsPage() {
  const navigate = useNavigate();
  const [section, setSection] = useState<SettingsSection>("providers");

  return (
    <div className="mx-auto flex h-full w-full max-w-5xl overflow-hidden rounded-lg border border-border bg-card shadow-sm animate-route-fade">
      <nav className="flex w-56 shrink-0 flex-col gap-1 border-r border-border p-3">
        <button
          type="button"
          onClick={() => navigate(-1)}
          className="mb-2 flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <ArrowLeft className="h-4 w-4" />
          Back
        </button>

        <p className="px-3 pb-1 text-xs font-semibold text-faint">Settings</p>

        {SECTIONS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            type="button"
            onClick={() => setSection(id)}
            aria-current={section === id}
            className={cn(
              "flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium transition-colors",
              section === id
                ? "bg-info-soft text-foreground"
                : "text-muted-foreground hover:bg-muted hover:text-foreground",
            )}
          >
            <Icon className="h-4 w-4" />
            {label}
          </button>
        ))}
      </nav>

      <div className="min-w-0 flex-1 overflow-y-auto p-5">
        <div key={section} className="animate-panel-in">
          {section === "providers" ? (
            <ProvidersPanel />
          ) : section === "credentials" ? (
            <GitHubCredentialsPanel />
          ) : (
            <SecurityPanel />
          )}
        </div>
      </div>
    </div>
  );
}
