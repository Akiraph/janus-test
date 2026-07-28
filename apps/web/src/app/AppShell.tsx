import { A, useLocation } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import Database from "lucide-solid/icons/database";
import Loader2 from "lucide-solid/icons/loader-2";
import Server from "lucide-solid/icons/server";
import Settings from "lucide-solid/icons/settings";
import ShieldCheck from "lucide-solid/icons/shield-check";
import type { JSX } from "solid-js";
import { Show, Suspense } from "solid-js";
import { JanusLogo } from "../components/JanusLogo";
import { type TabItem, Tabs } from "../components/ui/Tabs";
import { IdeShellScaffold } from "../features/projects/workspace/IdeShellScaffold";
import { useBootstrap, useMe } from "../lib/queries";
import { useEventStream } from "../lib/useEventStream";
import { LoginPage, SetupPage } from "../pages/AuthPages";
import "./app.css";

const MODE_TABS: TabItem[] = [
  { value: "code", label: "Code" },
  { value: "mtc", label: "MTC", disabled: true, badge: "soon" },
];

/** Inline route-loading marker for non-workspace routes while a lazy chunk
 *  resolves. Workspace routes use IdeShellScaffold instead so the IDE chrome
 *  never disappears into a bare spinner. */
function RouteLoading() {
  return (
    <div class="route-loading" role="status" aria-label="Loading">
      <Loader2 size={20} class="route-loading__spin" />
    </div>
  );
}

interface AppShellProps {
  children?: JSX.Element;
}

export function AppShell(props: AppShellProps) {
  const location = useLocation();
  const bootstrap = useBootstrap();
  const me = useMe();
  // Keep SSE live for project/git/operation invalidation across routes.
  useEventStream();
  const isImmersive = () => location.pathname.startsWith("/projects");
  const isSettings = () =>
    (location.pathname.startsWith("/settings") ||
      location.pathname.startsWith("/system") ||
      location.pathname.startsWith("/models") ||
      location.pathname.startsWith("/security")) &&
    !isImmersive();
  const isModels = () => location.pathname === "/settings" || location.pathname === "/models";

  // Bootstrap pending: the Setup/Login/normal decision needs it. We do NOT
  // return a full-screen splash here as a gate — if bootstrap ever hung (HMR
  // dirty state, a stalled fetch) it would freeze the whole app and block even
  // a refresh. Instead, fall through: the Suspense/Lazy layer below paints an
  // inline route-loading marker while chunks/queries resolve, and mode
  // selection just re-runs the instant bootstrap lands.
  if (bootstrap.data?.data.state === "uninitialized" && !bootstrap.data?.data.development_auth)
    return <SetupPage />;
  if (
    bootstrap.data?.data.state === "initialized" &&
    me.isError &&
    !bootstrap.data?.data.development_auth
  )
    return <LoginPage />;

  return (
    <Show
      when={!isImmersive()}
      fallback={
        <main class="home-route home-route--immersive">
          {/* ProjectPage is lazy + its createQueries suspend on cache miss.
              Fallback must keep the IDE chrome (rail/sidebar/topbar), not a bare
              spinner — otherwise every session open looks like a full reload. */}
          <Suspense fallback={<IdeShellScaffold />}>{props.children}</Suspense>
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
                  <ArrowLeft size={16} />
                  Back
                </A>
                <p>Settings</p>
                <A href="/system" classList={{ active: location.pathname === "/system" }}>
                  <Database size={16} />
                  System
                </A>
                <A href="/settings" classList={{ active: isModels() }}>
                  <Server size={16} />
                  Model Providers
                </A>
                <A href="/security" classList={{ active: location.pathname === "/security" }}>
                  <ShieldCheck size={16} />
                  Security
                </A>
              </nav>
              <div class="settings-content">
                <main class="settings-route">
                  <Suspense fallback={<RouteLoading />}>{props.children}</Suspense>
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
                <A class="settings-link" href="/settings">
                  <Settings size={16} />
                  Settings
                </A>
              </div>
            </div>
          </header>
          <main class="home-route">
            <Suspense fallback={<RouteLoading />}>{props.children}</Suspense>
          </main>
        </div>
      </Show>
    </Show>
  );
}
