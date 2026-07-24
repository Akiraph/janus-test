import { CheckCircle2, Info, TriangleAlert, X, XCircle } from "lucide-solid";
import { For } from "solid-js";
import { Portal } from "solid-js/web";
import type { NotificationVariant } from "./notifications";
import { useNotifications } from "./notifications";

const ICONS: Record<NotificationVariant, typeof Info> = {
  info: Info,
  success: CheckCircle2,
  warning: TriangleAlert,
  danger: XCircle,
};

export function NotificationContainer() {
  const store = useNotifications();
  return (
    <Portal>
      <aside class="ui-notifications" aria-label="Notifications" aria-live="polite">
        <For each={store.list()}>
          {(item) => {
            const Icon = ICONS[item.variant];
            return (
              <div class="ui-notification" data-variant={item.variant} role="status">
                <Icon size={18} class="ui-notification__icon" />
                <span class="ui-notification-message">{item.message}</span>
                <button
                  type="button"
                  class="ui-notification-close"
                  aria-label="Dismiss"
                  onClick={() => store.dismiss(item.id)}
                >
                  <X size={16} />
                </button>
              </div>
            );
          }}
        </For>
      </aside>
    </Portal>
  );
}
