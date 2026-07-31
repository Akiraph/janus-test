import CheckCircle2 from "lucide-solid/icons/check-circle-2";
import ChevronRight from "lucide-solid/icons/chevron-right";
import CircleDashed from "lucide-solid/icons/circle-dashed";
import Database from "lucide-solid/icons/database";
import Radio from "lucide-solid/icons/radio";
import Server from "lucide-solid/icons/server";
import { For, Match, Switch } from "solid-js";
import { NotificationEvent } from "../../components/ui/notifications";
import { useSystemInfo } from "../../lib/queries";
import "./system.css";

export function SystemStatus() {
  const system = useSystemInfo();

  return (
    <section class="panel" aria-labelledby="system-title">
      <NotificationEvent
        message={system.isError ? "System status unavailable" : null}
        variant="danger"
        action={{ label: "Retry", onClick: () => void system.refetch() }}
      />
      <div class="panel-heading">
        <h2 id="system-title">System</h2>
        <p>Deployment status and capabilities</p>
      </div>

      <Switch>
        <Match when={system.isPending}>
          <p class="surface-note" role="status" aria-label="Loading...">
            Loading...
          </p>
        </Match>
        <Match when={system.data}>
          {(response) => {
            const data = () => response().data;
            return (
              <div class="system-columns">
                <div class="system-section">
                  <h2>Service</h2>
                  <div class="detail-list">
                    <DetailRow icon={Server} label="Version" value={data().version} />
                    <DetailRow
                      icon={Database}
                      label="Schema"
                      value={String(data().schema_version)}
                    />
                    <DetailRow
                      icon={Radio}
                      label="Event range"
                      value={`${data().events.min_cursor} - ${data().events.max_cursor}`}
                    />
                  </div>
                </div>

                <div class="system-section">
                  <h2>Capabilities</h2>
                  <div class="capability-list">
                    <For each={data().capabilities}>
                      {(capability) => (
                        <div class="capability-row">
                          <span
                            class={
                              capability.state === "ready" ? "capability-ready" : "capability-muted"
                            }
                          >
                            {capability.state === "ready" ? (
                              <CheckCircle2 size={16} />
                            ) : (
                              <CircleDashed size={16} />
                            )}
                          </span>
                          <span>{formatCapability(capability.id)}</span>
                          <code>{capability.state}</code>
                          <ChevronRight size={15} class="row-chevron" />
                        </div>
                      )}
                    </For>
                  </div>
                </div>
              </div>
            );
          }}
        </Match>
      </Switch>
    </section>
  );
}

interface DetailRowProps {
  icon: typeof Server;
  label: string;
  value: string;
}

function DetailRow(props: DetailRowProps) {
  return (
    <div class="detail-row">
      <props.icon size={17} />
      <span>{props.label}</span>
      <strong>{props.value}</strong>
    </div>
  );
}

function formatCapability(id: string): string {
  return id
    .replace("delegated_cli.", "")
    .split("_")
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(" ");
}
