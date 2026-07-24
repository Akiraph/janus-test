import type { JSX } from "solid-js";
import { createContext, createSignal, useContext } from "solid-js";

export type NotificationVariant = "info" | "success" | "warning" | "danger";

export interface NotificationItem {
  id: number;
  message: string;
  variant: NotificationVariant;
  duration: number;
}

export interface NotificationStore {
  list: () => NotificationItem[];
  notify: (message: string, opts?: { variant?: NotificationVariant; duration?: number }) => void;
  dismiss: (id: number) => void;
}

const NotificationContext = createContext<NotificationStore>();

let nextId = 1;

export function NotificationProvider(props: { children: JSX.Element }) {
  const [items, setItems] = createSignal<NotificationItem[]>([]);

  const dismiss = (id: number) => {
    setItems((current) => current.filter((item) => item.id !== id));
  };

  const notify: NotificationStore["notify"] = (message, opts) => {
    const item: NotificationItem = {
      id: nextId++,
      message,
      variant: opts?.variant ?? "info",
      duration: opts?.duration ?? 4000,
    };
    setItems((current) => [...current, item]);
    if (item.duration > 0) {
      setTimeout(() => dismiss(item.id), item.duration);
    }
  };

  const store: NotificationStore = { list: items, notify, dismiss };
  return (
    <NotificationContext.Provider value={store}>{props.children}</NotificationContext.Provider>
  );
}

export function useNotifications(): NotificationStore {
  const store = useContext(NotificationContext);
  if (!store) throw new Error("useNotifications must be used inside a NotificationProvider");
  return store;
}
