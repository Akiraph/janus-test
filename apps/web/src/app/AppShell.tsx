import { A, useLocation } from "@solidjs/router";
import { ArrowLeft, Database, Server, Settings, ShieldCheck } from "lucide-solid";
import type { JSX } from "solid-js";
import { Show, Suspense } from "solid-js";
import { JanusLogo } from "../components/JanusLogo";
import { Skeleton } from "../components/ui/Skeleton";
import { type TabItem, Tabs } from "../components/ui/Tabs";
import { LoginPage, SetupPage } from "../features/auth/AuthPage";
import { useBootstrap, useMe } from "../lib/queries";
import { useEventStream } from "../lib/useEventStream";

const MODE_TABS: TabItem[] = [
  { value: "code", label: "Code" },
  { value: "mtc", label: "MTC", disabled: true, badge: "soon" },
];

interface AppShellProps {
  children?: JSX.Element;
}

export function AppShell(props: AppShellProps) {
  const location = useLocation();
  const bootstrap = useBootstrap();
  const me = useMe();
  // Keep SSE live for project/git/operation invalidation across routes.
  useEventStream();
  const isSettings = () =>
    location.pathname.startsWith("/settings") ||
    location.pathname.startsWith("/system") ||
    location.pathname.startsWith("/models") ||
    location.pathname.startsWith("/security");
  const isModels = () => location.pathname === "/settings" || location.pathname === "/models";

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
                <Suspense fallback={<Skeleton aria-label="Loading route" />}>
                  {props.children}
                </Suspense>
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
          <Suspense fallback={<Skeleton aria-label="Loading route" />}>{props.children}</Suspense>
        </main>
      </div>
    </Show>
  );
}
