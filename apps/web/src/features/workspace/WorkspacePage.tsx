import { Database, FolderGit2, Radio, Server } from "lucide-solid";
import { For, Match, Switch } from "solid-js";
import { QueryError, QuerySkeleton } from "../../components/QueryState";
import { useBootstrap, useSystemInfo } from "../../lib/queries";

export function WorkspacePage() {
  const bootstrap = useBootstrap();
  const system = useSystemInfo();

  return (
    <div class="workspace-layout route-enter">
      <section class="workspace-main" aria-labelledby="workspace-title">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Workspace</p>
            <h1 id="workspace-title">Projects</h1>
          </div>
          <Switch>
            <Match when={bootstrap.isPending}>
              <span class="status-chip muted">Checking</span>
            </Match>
            <Match when={bootstrap.data}>
              <span class="status-chip success">Ready</span>
            </Match>
          </Switch>
        </div>

        <div class="empty-workspace">
          <span class="empty-icon">
            <FolderGit2 size={28} strokeWidth={1.5} />
          </span>
          <h2>No projects</h2>
          <p>The workspace is ready.</p>
        </div>
      </section>

      <aside class="system-rail" aria-labelledby="control-plane-title">
        <div class="section-heading compact">
          <div>
            <p class="eyebrow">Local service</p>
            <h2 id="control-plane-title">Control plane</h2>
          </div>
        </div>

        <Switch>
          <Match when={system.isPending}>
            <QuerySkeleton />
          </Match>
          <Match when={system.isError}>
            <QueryError retry={() => void system.refetch()} />
          </Match>
          <Match when={system.data}>
            {(response) => {
              const data = () => response().data;
              const rows = () => [
                { icon: Server, label: "Server", value: `v${data().version}` },
                {
                  icon: Database,
                  label: "Database",
                  value: data().database.journal_mode.toUpperCase(),
                },
                { icon: Radio, label: "Event cursor", value: data().events.max_cursor },
              ];
              return (
                <div class="status-list">
                  <For each={rows()}>
                    {(row) => (
                      <div class="status-row">
                        <span class="status-row-icon">
                          <row.icon size={16} />
                        </span>
                        <span class="status-row-label">{row.label}</span>
                        <strong>{row.value}</strong>
                      </div>
                    )}
                  </For>
                  <div class="service-summary">
                    <span class="live-dot" />
                    <span>Operational</span>
                    <code>{data().mode}</code>
                  </div>
                </div>
              );
            }}
          </Match>
        </Switch>
      </aside>
    </div>
  );
}
