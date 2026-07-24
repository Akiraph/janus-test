import "@fontsource-variable/figtree";
import "@fontsource-variable/cascadia-code";
import { QueryClient, QueryClientProvider } from "@tanstack/solid-query";
import { render } from "solid-js/web";
import { App } from "./app/App";
import { NotificationContainer } from "./components/ui/NotificationContainer";
import { NotificationProvider } from "./components/ui/notifications";
import { ApiError } from "./lib/api";
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Retry only transient/server or network failures; never retry client
      // errors (4xx) so a missing endpoint doesn't burn seconds on retries.
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
          return false;
        }
        return failureCount < 1;
      },
      staleTime: 30_000,
      refetchOnWindowFocus: false,
    },
  },
});

const root = document.getElementById("root");
if (!root) {
  throw new Error("Janus root element is missing");
}

render(
  () => (
    <QueryClientProvider client={queryClient}>
      <NotificationProvider>
        <App />
        <NotificationContainer />
      </NotificationProvider>
    </QueryClientProvider>
  ),
  root,
);
