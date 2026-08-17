import Database from "lucide-solid/icons/database";
import Radio from "lucide-solid/icons/radio";
import Server from "lucide-solid/icons/server";
import { Match, Switch } from "solid-js";
import { NotificationEvent } from "../../components/ui/notifications";
import { getErrorMessage } from "../../lib/api";
import { useSystemInfo } from "../../lib/queries";
import "./system.css";

export function SystemStatus() {
  const system = useSystemInfo();

  return (
    <section class="panel" aria-labelledby="system-title">
      <NotificationEvent
        message={system.isError ? getErrorMessage(system.error, "System status unavailable") : null}
        variant="danger"
        action={{ label: "Retry", onClick: () => void system.refetch() }}
      />
      <div class="panel-heading">
        <h2 id="system-title">System</h2>
        <p>Deployment status</p>
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
              <div class="system-section">
                <h2>Service</h2>
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
      <props.icon size={17} />
      <span>{props.label}</span>
      <strong>{props.value}</strong>
    </div>
  );
}
