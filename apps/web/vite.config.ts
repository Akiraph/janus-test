import { defineConfig, loadEnv } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "");
  const webPort = Number(env.JANUS_WEB_PORT ?? "5173");
  const apiTarget = env.JANUS_API_TARGET ?? "http://127.0.0.1:4317";

  if (!Number.isInteger(webPort) || webPort < 1 || webPort > 65_535) {
    throw new Error(`JANUS_WEB_PORT must be a valid TCP port: ${env.JANUS_WEB_PORT}`);
  }

  return {
    plugins: [solid()],
    // Pre-bundle the heaviest deps so dev cold-visits of routes don't stall on
    // on-the-fly transpilation. vite-plugin-solid already handles solid-js; we
    // add the router, query, and icon libraries that lazy route components pull
    // in bulk (lucide-solid especially — named icon imports otherwise force vite
    // to crawl hundreds of tiny icon modules per cold visit).
    optimizeDeps: {
      include: ["@solidjs/router", "@tanstack/solid-query", "lucide-solid"],
    },
    server: {
      port: webPort,
      strictPort: true,
      proxy: {
        "/api": { target: apiTarget, ws: true },
        "/health": apiTarget,
      },
      // Warm up the two most-used entry points as soon as the server starts, so
      // the very first browser visit already has them transpiled and cached.
      warmup: {
        clientFiles: [
          "./src/main.tsx",
          "./src/app/App.tsx",
          "./src/features/projects/ProjectsOverview.tsx",
          "./src/features/projects/ProjectWorkspace.tsx",
        ],
      },
    },
    build: {
      target: "es2022",
    },
  };
});
