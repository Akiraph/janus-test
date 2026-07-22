import { A, useLocation } from "@solidjs/router";
import { Activity, PanelsTopLeft, Settings, ShieldAlert } from "lucide-solid";
import type { JSX } from "solid-js";
import { Show } from "solid-js";
import { useBootstrap } from "../lib/queries";
import { useEventStream } from "../lib/useEventStream";

interface AppShellProps {
  children?: JSX.Element;
}

const connectionLabels = {
  connecting: "Connecting",
  live: "Live",
  reconnecting: "Reconnecting",
  offline: "Offline",
} as const;

export function AppShell(props: AppShellProps) {
  const location = useLocation();
  const bootstrap = useBootstrap();
  const connection = useEventStream();

  return (
    <div class="app-canvas">
      <div class="app-frame">
        <header class="topbar">
          <A class="brand" href="/" aria-label="Janus workspace">
            <span class="brand-mark">
              <PanelsTopLeft size={17} strokeWidth={1.8} />
            </span>
            <span>Janus</span>
          </A>

          <nav class="topnav" aria-label="Primary navigation">
            <A href="/" classList={{ active: location.pathname === "/" }} end>
              Workspace
            </A>
            <A href="/system" classList={{ active: location.pathname === "/system" }}>
              System
            </A>
          </nav>

          <div class="topbar-actions">
            <span class={`connection connection-${connection()}`}>
              <Activity size={14} aria-hidden="true" />
              {connectionLabels[connection()]}
            </span>
            <A
              class="icon-button"
              href="/system"
              aria-label="System settings"
              title="System settings"
            >
              <Settings size={17} />
            </A>
          </div>
        </header>

        <Show when={bootstrap.data?.data.development_auth}>
          <div class="dev-banner" role="status">
            <ShieldAlert size={15} aria-hidden="true" />
            <span>Development authentication</span>
          </div>
        </Show>

        <main class="route-content">{props.children}</main>
      </div>
    </div>
  );
}
