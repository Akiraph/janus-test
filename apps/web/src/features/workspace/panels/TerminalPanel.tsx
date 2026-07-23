import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { TerminalSquare } from "lucide-react";
import { useEffect, useRef, useState } from "react";

interface TerminalPanelProps {
  readonly projectId: string;
  readonly terminalId: string;
}

type ConnState = "connecting" | "live" | "unavailable";

/**
 * Terminal theme resolved from the design-token CSS variables (:root) so the
 * terminal tracks the rest of the UI instead of carrying its own hex palette.
 * xterm expects resolved colors, so we read the computed HSL triplets here.
 */
function resolveTerminalTheme() {
  const styles = getComputedStyle(document.documentElement);
  const hsl = (name: string) => styles.getPropertyValue(name).trim();
  const toHex = (channels: string): string | undefined => {
    const parts = channels.split(/\s+/).map((n) => Number(n));
    const [h, s, l] = parts;
    if (![h, s, l].every((v) => Number.isFinite(v))) {
      return undefined;
    }
    return hslToHex(h as number, s as number, l as number);
  };
  return {
    background: toHex(hsl("--card")) ?? "#ffffff",
    foreground: toHex(hsl("--foreground")) ?? "#1e2530",
    cursor: toHex(hsl("--border-accent")) ?? "#7fccec",
    cursorAccent: toHex(hsl("--card")) ?? "#ffffff",
    selectionBackground: toHex(hsl("--border-accent-soft")) ?? "#cdf0f5",
  };
}

function hslToHex(h: number, s: number, l: number): string {
  const sl = s / 100;
  const ll = l / 100;
  const c = (1 - Math.abs(2 * ll - 1)) * sl;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = ll - c / 2;
  let r = 0;
  let g = 0;
  let b = 0;
  if (h < 60) [r, g, b] = [c, x, 0];
  else if (h < 120) [r, g, b] = [x, c, 0];
  else if (h < 180) [r, g, b] = [0, c, x];
  else if (h < 240) [r, g, b] = [0, x, c];
  else if (h < 300) [r, g, b] = [x, 0, c];
  else [r, g, b] = [c, 0, x];
  const to = (v: number) =>
    Math.round((v + m) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

const ESC = "";

/**
 * TerminalPanel — interactive terminal for the project sandbox.
 *
 * Connects to the backend project-terminal WebSocket at
 * `/api/projects/:projectId/terminal`.
 */
export function TerminalPanel({ projectId, terminalId }: TerminalPanelProps) {
  return (
    <InteractiveTerminalPanel projectId={projectId} terminalId={terminalId} />
  );
}

function InteractiveTerminalPanel({
  projectId,
  terminalId,
}: {
  readonly projectId: string;
  readonly terminalId: string;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<ConnState>("connecting");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    setState("connecting");

    const term = new Terminal({
      fontFamily:
        '"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: resolveTerminalTheme(),
      convertEol: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();

    const resizeObserver = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        // Ignore fit errors during teardown.
      }
    });
    resizeObserver.observe(container);

    let socket: WebSocket | null = null;
    let disposed = false;
    // Track connection phase locally so the effect never reads React state
    // (avoids stale-closure bugs); setPhase mirrors it to state for the badge.
    let phase: ConnState = "connecting";
    const setPhase = (next: ConnState) => {
      phase = next;
      if (!disposed) setState(next);
    };

    term.onData((data) => {
      if (phase === "live") {
        socket?.send(data);
      }
      // "connecting": swallow input until a transport is ready.
      // "unavailable": the terminal is read-only until the tab reconnects.
    });

    const markUnavailable = (message: string) => {
      if (disposed || phase !== "connecting") return;
      setPhase("unavailable");
      term.writeln(`${ESC}[31m${message}${ESC}[0m`);
    };

    const connect = () => {
      let wsUrl: string;
      try {
        const httpUrl = new URL(
          `${resolveTerminalApiBase()}/api/projects/${encodeURIComponent(
            projectId,
          )}/terminal`,
          window.location.origin,
        );
        httpUrl.searchParams.set("terminalId", terminalId);
        httpUrl.protocol = httpUrl.protocol === "https:" ? "wss:" : "ws:";
        wsUrl = httpUrl.toString();
      } catch {
        markUnavailable("Terminal URL could not be resolved.");
        return;
      }

      // Give the connection a short window; then report the real issue.
      const fallbackTimer = setTimeout(() => {
        if (phase === "connecting") {
          socket?.close();
          markUnavailable("Terminal backend is not reachable.");
        }
      }, 1200);

      try {
        socket = new WebSocket(wsUrl);
      } catch {
        clearTimeout(fallbackTimer);
        markUnavailable("Terminal backend is not reachable.");
        return;
      }

      socket.binaryType = "arraybuffer";
      socket.onopen = () => {
        clearTimeout(fallbackTimer);
        if (disposed) return;
        setPhase("live");
      };
      socket.onmessage = (event) => {
        const data = event.data;
        if (typeof data === "string") {
          term.write(data);
        } else {
          term.write(new Uint8Array(data as ArrayBuffer));
        }
      };
      socket.onerror = () => {
        clearTimeout(fallbackTimer);
        if (!disposed && phase !== "live") {
          markUnavailable("Terminal backend is not reachable.");
        }
      };
      socket.onclose = () => {
        clearTimeout(fallbackTimer);
        if (!disposed && phase === "connecting") {
          markUnavailable("Terminal backend is not reachable.");
        } else if (!disposed && phase === "live") {
          setPhase("unavailable");
          term.writeln(`\r\n${ESC}[31mTerminal connection closed.${ESC}[0m`);
        }
      };
    };

    connect();

    return () => {
      disposed = true;
      resizeObserver.disconnect();
      socket?.close();
      term.dispose();
    };
  }, [projectId, terminalId]);

  return (
    <div className="flex h-full flex-col bg-card">
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-border px-3">
        <TerminalSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
        <span className="text-xs font-medium text-faint">Terminal</span>
        <ConnBadge state={state} />
      </div>
      <div
        ref={containerRef}
        className="min-h-0 flex-1 overflow-hidden p-2 pt-3"
      />
    </div>
  );
}

function resolveTerminalApiBase(): string {
  const configuredBase = (import.meta.env.VITE_API_BASE_URL ?? "").trim();

  if (configuredBase.length > 0) {
    return configuredBase.replace(/\/+$/g, "");
  }

  if (window.location.port === "5173") {
    const devServer = new URL(window.location.origin);
    devServer.protocol = "http:";
    devServer.port = "4317";
    return devServer.toString().replace(/\/+$/g, "");
  }

  return window.location.origin;
}

function ConnBadge({ state }: { state: ConnState }) {
  const map = {
    connecting: { label: "connecting…", cls: "bg-warning-soft text-warning" },
    live: { label: "live", cls: "bg-success-soft text-success" },
    unavailable: {
      label: "unavailable",
      cls: "bg-muted text-muted-foreground",
    },
  } as const;
  const { label, cls } = map[state];
  return (
    <span
      className={`rounded-sm px-1.5 py-0.5 text-[10px] font-medium transition-colors duration-150 ${cls}`}
    >
      {label}
    </span>
  );
}
