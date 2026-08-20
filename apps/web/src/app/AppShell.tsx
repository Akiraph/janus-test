import { A, useLocation } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import Bell from "lucide-solid/icons/bell";
import Database from "lucide-solid/icons/database";
import GitPullRequest from "lucide-solid/icons/git-pull-request";
import Loader2 from "lucide-solid/icons/loader-2";
import Server from "lucide-solid/icons/server";
import Settings from "lucide-solid/icons/settings";
import ShieldCheck from "lucide-solid/icons/shield-check";
import type { JSX } from "solid-js";
import { Match, Show, Suspense, Switch } from "solid-js";
import { JanusLogo } from "../components/JanusLogo";
import { Button } from "../components/ui/Button";
import { type TabItem, Tabs } from "../components/ui/Tabs";
import { LoginView, SetupView } from "../features/auth/AuthViews";
import { IdeShellScaffold } from "../features/projects/workspace/IdeShellScaffold";
import { useBootstrap, useMe } from "../lib/queries";
import { useEventStream } from "../lib/useEventStream";
import "./app.css";

const MODE_TABS: TabItem[] = [
  { value: "code", label: "Code" },
  { value: "mtc", label: "MTC", disabled: true },
];

/** Inline route-loading marker for non-workspace routes while a lazy chunk
 *  resolves. Workspace routes use IdeShellScaffold instead so the IDE chrome
 *  never disappears into a bare spinner. */
function RouteLoading() {
  return (
    <div class="route-loading" role="status">
      <Loader2 size={20} class="ui-spinner" aria-hidden="true" />
      <span class="sr-only">Loading</span>
    </div>
  );
}

interface AppShellProps {
  children?: JSX.Element;
}

export function AppShell(props: AppShellProps) {
  const bootstrap = useBootstrap();

  return (
    <Switch
      fallback={
        <main class="route-loading">
          <div class="route-error" role="alert">
            <span>
              Unable to load Janus deployment state. Check that the Janus server is running.
            </span>
            <Button
              variant="outline"
              size="sm"
              disabled={bootstrap.isFetching}
              onClick={() => void bootstrap.refetch()}
            >
              {bootstrap.isFetching ? "Retrying..." : "Retry"}
            </Button>
          </div>
        </main>
      }
    >
      <Match when={bootstrap.isPending}>
        <RouteLoading />
      </Match>
      <Match
        when={
          bootstrap.data?.data.state === "uninitialized" && !bootstrap.data.data.development_auth
        }
      >
        <SetupView />
      </Match>
      <Match when={bootstrap.data?.data.development_auth}>
        <AuthenticatedShell route={() => props.children} />
      </Match>
      <Match when={bootstrap.data?.data.state === "initialized"}>
        <OwnerGate route={() => props.children} />
      </Match>
    </Switch>
  );
}

interface RoutedShellProps {
  route: () => JSX.Element | undefined;
}

function OwnerGate(props: RoutedShellProps) {
  const me = useMe();

  return (
    <Switch fallback={<RouteLoading />}>
      <Match when={me.isPending}>
        <RouteLoading />
      </Match>
      <Match when={me.isError}>
        <LoginView />
      </Match>
      <Match when={me.isSuccess}>
        <AuthenticatedShell route={props.route} />
      </Match>
    </Switch>
  );
}

function AuthenticatedShell(props: RoutedShellProps) {
  const location = useLocation();
  // Keep SSE live for project/git/operation invalidation across routes.
  useEventStream();
  const isImmersive = () => location.pathname.startsWith("/projects");
  const isSettings = () =>
    (location.pathname.startsWith("/settings") ||
      location.pathname.startsWith("/system") ||
      location.pathname.startsWith("/models") ||
      location.pathname.startsWith("/security") ||
      location.pathname === "/notifications" ||
      location.pathname.startsWith("/automation")) &&
    !isImmersive();
  const isModels = () => location.pathname === "/settings" || location.pathname === "/models";
  const isNotifications = () => location.pathname === "/notifications";
  const isAutomation = () => location.pathname.startsWith("/automation");

  return (
    <Show
      when={!isImmersive()}
      fallback={
        <main class="home-route home-route--immersive">
          <Suspense fallback={<IdeShellScaffold />}>{props.route()}</Suspense>
        </main>
      }
    >
      <Show
        when={!isSettings()}
        fallback={
          <div class="settings-canvas">
            <div class="settings-shell">
              <nav class="settings-rail" aria-label="Settings navigation">
                <A class="settings-back" href="/" end>
                  <ArrowLeft size={16} aria-hidden="true" />
                  Back
                </A>
                <p>Settings</p>
                <A href="/system" classList={{ active: location.pathname === "/system" }}>
                  <Database size={16} aria-hidden="true" />
                  System
                </A>
                <A href="/settings" classList={{ active: isModels() }}>
                  <Server size={16} aria-hidden="true" />
                  Model Providers
                </A>
                <A href="/security" classList={{ active: location.pathname === "/security" }}>
                  <ShieldCheck size={16} aria-hidden="true" />
                  Security
                </A>
                <A href="/notifications" classList={{ active: isNotifications() }}>
                  <Bell size={16} aria-hidden="true" />
                  Notifications
                </A>
                <A href="/automation" classList={{ active: isAutomation() }}>
                  <GitPullRequest size={16} aria-hidden="true" />
                  Automation
                </A>
              </nav>
              <div class="settings-content">
                <main class="settings-route">
                  <Suspense fallback={<RouteLoading />}>{props.route()}</Suspense>
                </main>
              </div>
            </div>
          </div>
        }
      >
        <div class="home-canvas">
          <header class="home-bar">
            <div class="home-bar-inner">
              <div class="home-bar-left">
                <A class="brand" href="/" aria-label="Janus workspace">
                  <JanusLogo size={28} />
                  <span>Janus</span>
                </A>
                <Tabs
                  value="code"
                  onChange={() => {
                    /* 多 mode 时接路由 */
                  }}
                  tabs={MODE_TABS}
                  aria-label="Product mode"
                />
              </div>
              <div class="topbar-actions">
                <A class="settings-link" href="/automation">
                  <GitPullRequest size={16} aria-hidden="true" />
                  Automation
                </A>
                <A class="settings-link" href="/settings">
                  <Settings size={16} aria-hidden="true" />
                  Settings
                </A>
              </div>
            </div>
          </header>
          <main class="home-route">
            <Suspense fallback={<RouteLoading />}>{props.route()}</Suspense>
          </main>
        </div>
      </Show>
    </Show>
  );
}
