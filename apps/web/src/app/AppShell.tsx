import { A, useLocation } from "@solidjs/router";
import {
  Activity,
  ArrowLeft,
  Database,
  Server,
  Settings,
  ShieldAlert,
  ShieldCheck,
} from "lucide-solid";
import type { JSX } from "solid-js";
import { Show } from "solid-js";
import { JanusLogo } from "../components/JanusLogo";
import { LoginPage, SetupPage } from "../features/auth/AuthPage";
import { useBootstrap, useMe } from "../lib/queries";
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
  const me = useMe();
  const connection = useEventStream();
  const isSettings = () => location.pathname !== "/";

  if (bootstrap.data?.data.state === "uninitialized" && !bootstrap.data?.data.development_auth)
    return <SetupPage />;
  if (
    bootstrap.data?.data.state === "initialized" &&
    me.isError &&
    !bootstrap.data?.data.development_auth
  )
    return <LoginPage />;

  const banner = (
    <Show when={bootstrap.data?.data.development_auth}>
      <div class="dev-banner" role="status">
        <ShieldAlert size={15} aria-hidden="true" />
        <span>Development authentication</span>
      </div>
    </Show>
  );

  return (
    <Show
      when={!isSettings()}
      fallback={
        <div class="settings-canvas">
          <div class="legacy-settings-shell">
            <nav class="settings-nav" aria-label="Settings navigation">
              <A class="settings-back" href="/">
                <ArrowLeft size={16} />
                Back
              </A>
              <p>Settings</p>
              <A href="/system" classList={{ active: location.pathname === "/system" }}>
                <Database size={16} />
                System
              </A>
              <A href="/models" classList={{ active: location.pathname === "/models" }}>
                <Server size={16} />
                Model providers
              </A>
              <A href="/security" classList={{ active: location.pathname === "/security" }}>
                <ShieldCheck size={16} />
                Security
              </A>
              <span class={`settings-connection connection-${connection()}`}>
                <Activity size={14} />
                {connectionLabels[connection()]}
              </span>
            </nav>
            <div class="settings-content">
              {banner}
              <main class="settings-route">{props.children}</main>
            </div>
          </div>
        </div>
      }
    >
      <div class="home-canvas">
        <header class="legacy-topbar">
          <div class="legacy-nav-inner">
            <div class="legacy-nav-left">
              <A class="brand" href="/" aria-label="Janus workspace">
                <JanusLogo size={28} />
                <span>Janus</span>
              </A>
              <fieldset class="mode-switch">
                <legend class="sr-only">Product mode</legend>
                <span class="active">Code</span>
                <span class="disabled">
                  MTC <small>soon</small>
                </span>
              </fieldset>
            </div>
            <div class="topbar-actions">
              <span class={`connection connection-${connection()}`}>
                <Activity size={14} aria-hidden="true" />
                {connectionLabels[connection()]}
              </span>
              <A class="settings-link" href="/models">
                <Settings size={16} />
                Settings
              </A>
            </div>
          </div>
        </header>
        {banner}
        <main class="home-route">{props.children}</main>
      </div>
    </Show>
  );
}
