import type { JSX } from "solid-js";
import { createContext, createEffect, createSignal, useContext } from "solid-js";

export type NotificationVariant = "info" | "success" | "warning" | "danger";

export interface NotificationItem {
  id: number;
  message: string;
  variant: NotificationVariant;
  duration: number;
  action?: NotificationAction;
}

export interface NotificationAction {
  label: string;
  onClick: () => void;
}

export interface NotificationOptions {
  variant?: NotificationVariant;
  duration?: number;
  action?: NotificationAction;
}

export interface NotificationStore {
  list: () => NotificationItem[];
  notify: (message: string, opts?: NotificationOptions) => void;
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
      duration: opts?.duration ?? (opts?.action ? 0 : 4000),
      ...(opts?.action ? { action: opts.action } : {}),
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

interface NotificationEventProps extends NotificationOptions {
  message: string | null | undefined;
}

/** Emits once for each non-empty error value without occupying layout space. */
export function NotificationEvent(props: NotificationEventProps) {
  const notify = useNotifications().notify;
  let previousMessage: string | null = null;

  createEffect(() => {
    const message = props.message?.trim() ?? "";
    if (!message) {
      previousMessage = null;
      return;
    }
    if (message === previousMessage) return;
    previousMessage = message;
    notify(message, {
      ...(props.variant ? { variant: props.variant } : {}),
      ...(props.duration !== undefined ? { duration: props.duration } : {}),
      ...(props.action ? { action: props.action } : {}),
    });
  });

  return null;
}
