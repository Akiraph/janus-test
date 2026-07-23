import type { IncomingMessage, ServerResponse } from "node:http";
import type { Socket } from "node:net";
import react from "@vitejs/plugin-react";
import { defineConfig, type ProxyOptions } from "vite";

type ProxyServer = Parameters<NonNullable<ProxyOptions["configure"]>>[0];
type ProxyErrorListener = (
  error: Error,
  request: IncomingMessage,
  response: ServerResponse | Socket,
) => void;

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://localhost:4317",
        ws: true,
        configure: silenceExpectedSseProxyClose,
      },
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules")) {
            if (id.includes("@codemirror/language-data")) {
              return "codemirror-language-data";
            }
            const codemirrorLanguage = id.match(
              /[\\/]@codemirror[\\/](lang-[^\\/]+)/,
            );
            if (codemirrorLanguage?.[1]) {
              return `codemirror-${codemirrorLanguage[1]}`;
            }
            const codemirrorPackage = id.match(
              /[\\/]@codemirror[\\/]([^\\/]+)/,
            );
            if (codemirrorPackage?.[1]) {
              return `codemirror-${codemirrorPackage[1]}`;
            }
            if (id.includes("@uiw/")) {
              return "codemirror-ui";
            }
            if (id.includes("@xterm/")) {
              return "terminal-vendor";
            }
            if (
              id.includes("react-markdown") ||
              id.includes("remark-gfm") ||
              id.includes("micromark") ||
              id.includes("mdast") ||
              id.includes("hast") ||
              id.includes("unified")
            ) {
              return "markdown-vendor";
            }
            if (id.includes("@tanstack/react-query")) {
              return "query-vendor";
            }
            if (id.includes("lucide-react")) {
              return "icons-vendor";
            }
            if (
              id.includes("react") ||
              id.includes("react-dom") ||
              id.includes("react-router")
            ) {
              return "react-vendor";
            }
            if (id.includes("@radix-ui")) {
              return "radix-vendor";
            }
            return "vendor";
          }
        },
      },
    },
    chunkSizeWarningLimit: 650,
  },
});

function silenceExpectedSseProxyClose(proxy: ProxyServer): void {
  const originalOn = proxy.on.bind(proxy);

  proxy.on = ((eventName, listener) => {
    if (eventName !== "error") {
      return originalOn(eventName, listener);
    }

    const wrappedListener: ProxyErrorListener = (error, request, response) => {
      if (isExpectedSseProxyClose(error, request, response)) {
        return;
      }

      (listener as unknown as ProxyErrorListener)(error, request, response);
    };

    return originalOn(eventName, wrappedListener as unknown as typeof listener);
  }) as typeof proxy.on;
}

function isExpectedSseProxyClose(
  error: Error,
  request: IncomingMessage,
  response: ServerResponse | Socket,
): boolean {
  return (
    isConnectionReset(error) &&
    isStreamPath(request.url) &&
    (request.destroyed || response.destroyed)
  );
}

function isConnectionReset(error: Error): boolean {
  return (
    ("code" in error && error.code === "ECONNRESET") ||
    error.message === "socket hang up"
  );
}

function isStreamPath(path: string | undefined): boolean {
  return path?.startsWith("/api/") === true && path.includes("-stream");
}
