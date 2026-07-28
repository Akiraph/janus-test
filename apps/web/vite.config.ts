import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
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
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": "http://127.0.0.1:4317",
      "/health": "http://127.0.0.1:4317",
    },
    // Warm up the two most-used entry points as soon as the server starts, so
    // the very first browser visit already has them transpiled and cached.
    warmup: {
      clientFiles: [
        "./src/main.tsx",
        "./src/app/App.tsx",
        "./src/features/projects/ProjectsPage.tsx",
        "./src/features/projects/ProjectPage.tsx",
      ],
    },
  },
  build: {
    target: "es2022",
  },
});
