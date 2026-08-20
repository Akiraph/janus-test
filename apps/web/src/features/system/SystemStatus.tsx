import Database from "lucide-solid/icons/database";
import Radio from "lucide-solid/icons/radio";
import Server from "lucide-solid/icons/server";
import { Match, Switch } from "solid-js";
import { Button } from "../../components/ui/Button";
import { getErrorMessage } from "../../lib/api";
import { useSystemInfo } from "../../lib/queries";
import "./system.css";

export function SystemStatus() {
  const system = useSystemInfo();

  return (
    <section class="panel" aria-labelledby="system-title">
      <div class="panel-heading">
        <h2 id="system-title">System</h2>
        <p>Deployment status</p>
      </div>

      <Switch>
        <Match when={system.isPending}>
          <p class="surface-note" role="status">
            Loading deployment status...
          </p>
        </Match>
        <Match when={system.isError && !system.data}>
          <div class="system-error" role="alert">
            <p>{getErrorMessage(system.error, "System status unavailable")}</p>
            <Button variant="outline" size="sm" onClick={() => void system.refetch()}>
              Retry
            </Button>
          </div>
        </Match>
        <Match when={system.data}>
          {(response) => {
            const data = () => response().data;
            return (
              <div class="system-section">
                <h3>Service</h3>
                <div class="detail-list">
                  <DetailRow icon={Server} label="Version" value={data().version} />
                  <DetailRow icon={Database} label="Schema" value={String(data().schema_version)} />
                  <DetailRow
                    icon={Radio}
                    label="Event range"
                    value={`${data().events.min_cursor} - ${data().events.max_cursor}`}
                  />
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
      <props.icon size={17} aria-hidden="true" />
      <span>{props.label}</span>
      <strong>{props.value}</strong>
    </div>
  );
}
