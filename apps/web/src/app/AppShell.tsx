import type { ReactNode } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { TopNav } from "../components/layout/TopNav";

export interface AppShellProps {
  readonly children: ReactNode;
}

/**
 * AppShell — Global layout container.
 * - Renders the global TopNav only on list-style pages (home). The workspace
 *   and settings pages own their full-height surface and provide their own
 *   navigation, so the global TopNav is hidden there.
 * - Home scrolls; the full-height pages clip and scroll internally.
 */
export function AppShell({ children }: AppShellProps) {
  const location = useLocation();
  const navigate = useNavigate();
  const isWorkspace = location.pathname.startsWith("/projects/");
  const isSettings = location.pathname.startsWith("/settings");
  const isAuthRoute =
    location.pathname.startsWith("/login") ||
    location.pathname.startsWith("/setup");
  const fullHeight = isWorkspace || isSettings || isAuthRoute;

  return (
    <div className="flex h-screen flex-col bg-background">
      {!fullHeight && (
        <div className="p-4 pb-0">
          <TopNav onSettingsClick={() => navigate("/settings")} />
        </div>
      )}

      <main
        className={
          fullHeight
            ? "flex-1 overflow-hidden p-4"
            : "flex-1 overflow-y-auto p-4"
        }
      >
        <div
          key={location.pathname}
          className={
            fullHeight ? "h-full animate-route-fade" : "animate-route-fade"
          }
        >
          {children}
        </div>
      </main>
    </div>
  );
}
