import CheckCircle2 from "lucide-solid/icons/check-circle-2";
import Info from "lucide-solid/icons/info";
import TriangleAlert from "lucide-solid/icons/triangle-alert";
import X from "lucide-solid/icons/x";
import XCircle from "lucide-solid/icons/x-circle";
import { For } from "solid-js";
import { Portal } from "solid-js/web";
import type { NotificationVariant } from "./notifications";
import "./notifications.css";
import { useNotifications } from "./notifications";

const ICONS: Record<NotificationVariant, typeof Info> = {
  info: Info,
  success: CheckCircle2,
  warning: TriangleAlert,
  danger: XCircle,
};

const VARIANT_LABEL: Record<NotificationVariant, string> = {
  info: "Note",
  success: "Success",
  warning: "Warning",
  danger: "Error",
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
              <div
                class="ui-notification"
                data-variant={item.variant}
                role={item.variant === "danger" ? "alert" : "status"}
              >
                <Icon size={18} class="ui-notification__icon" aria-hidden="true" />
                <span class="sr-only">{`${VARIANT_LABEL[item.variant]}: `}</span>
                <span class="ui-notification-message">{item.message}</span>
                {item.action ? (
                  <button
                    type="button"
                    class="ui-notification-action"
                    onClick={() => {
                      store.dismiss(item.id);
                      item.action?.onClick();
                    }}
                  >
                    {item.action.label}
                  </button>
                ) : null}
                <button
                  type="button"
                  class="ui-notification-close"
                  aria-label="Dismiss"
                  onClick={() => store.dismiss(item.id)}
                >
                  <X size={16} aria-hidden="true" />
                </button>
              </div>
            );
          }}
        </For>
      </aside>
    </Portal>
  );
}
