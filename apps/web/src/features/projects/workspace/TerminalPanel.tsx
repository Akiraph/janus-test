import { useQueryClient } from "@tanstack/solid-query";
import Loader2 from "lucide-solid/icons/loader-2";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Square from "lucide-solid/icons/square";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import X from "lucide-solid/icons/x";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { Button } from "../../../components/ui/Button";
import { EmptyState } from "../../../components/ui/EmptyState";
import { NotificationEvent } from "../../../components/ui/notifications";
import type { TerminalProjection, TerminalTicket } from "../../../lib/api";
import {
  closeTerminal,
  createTerminal,
  getErrorMessage,
  issueTerminalTicket,
  listTerminals,
  signalTerminal,
  terminalConnectUrl,
} from "../../../lib/api";
import { useIsMobile } from "../../../lib/viewport";

interface TerminalPanelProps {
  projectId: () => string | undefined;
  /** When false the panel is hidden (e.g. activity view not selected). */
  active?: () => boolean;
  /** Optional title override for the sidebar header. */
  title?: string;
}

type ConnectState =
  | "idle"
  | "loading"
  | "connecting"
  | "live"
  | "reconnecting"
  | "error"
  | "closed";

/** Lazy Terminal panel; xterm stays outside the initial bundle. */
export function TerminalPanel(props: TerminalPanelProps) {
  const queryClient = useQueryClient();
  const isMobile = useIsMobile();
  const [status, setStatus] = createSignal<ConnectState>("idle");
  const [error, setError] = createSignal("");
  const [terminal, setTerminal] = createSignal<TerminalProjection | null>(null);

  let hostEl: HTMLDivElement | undefined;
  // Emulator handles are intentionally untyped (`any`) here to keep the static
  // import graph free of @xterm. They are only assigned after the dynamic
  // import resolves; all calls are guarded by null checks.
  // biome-ignore lint/suspicious/noExplicitAny: lazy @xterm instance typed after dynamic import
  let term: any;
  // biome-ignore lint/suspicious/noExplicitAny: lazy @xterm addon typed after dynamic import
  let fitAddon: any;
  let socket: WebSocket | undefined;
  let disposed = false;
  let resizeObserver: ResizeObserver | undefined;
  let dataDisposable: { dispose: () => void } | undefined;

  function invalidateList() {
    const projectId = props.projectId();
    if (!projectId) return;
    void queryClient.invalidateQueries({ queryKey: ["terminals", projectId] });
  }

  async function ensureEmulator() {
    if (term || !hostEl) return;
    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
    ]);
    // Side-effect CSS import kept inside the dynamic chunk.
    await import("@xterm/xterm/css/xterm.css");
    if (disposed || !hostEl) return;

    const instance = new Terminal({
      cursorBlink: true,
      convertEol: true,
      fontFamily: "var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
      fontSize: 13,
      theme: {
        background: "#0d1117",
        foreground: "#e6edf3",
        cursor: "#e6edf3",
        selectionBackground: "rgba(56, 139, 253, 0.35)",
      },
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    instance.loadAddon(fit);
    instance.open(hostEl);
    fit.fit();
    term = instance;
    fitAddon = fit;

    dataDisposable = instance.onData((data: string) => {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(new TextEncoder().encode(data));
      }
    });

    resizeObserver = new ResizeObserver(() => {
      try {
        fit.fit();
        const cols = instance.cols;
        const rows = instance.rows;
        if (socket && socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ kind: "resize", cols, rows }));
        }
      } catch {
        // Fit can throw while the host is display:none; ignore.
      }
    });
    resizeObserver.observe(hostEl);
  }

  function tearDownSocket() {
    if (socket) {
      try {
        socket.close();
      } catch {
        // ignore
      }
      socket = undefined;
    }
  }

  async function connectTo(projection: TerminalProjection) {
    setTerminal(projection);
    setStatus("connecting");
    setError("");
    await ensureEmulator();
    if (disposed || !term) return;

    let ticket: TerminalTicket | undefined;
    try {
      ticket = await issueTerminalTicket(projection.id);
    } catch (err) {
      setStatus("error");
      setError(getErrorMessage(err, "Failed to issue terminal ticket"));
      return;
    }
    if (disposed) return;

    tearDownSocket();
    if (!ticket) return;
    const url = terminalConnectUrl(projection.id, ticket.token);
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    socket = ws;

    ws.addEventListener("open", () => {
      if (disposed) return;
      setStatus("live");
      try {
        fitAddon?.fit();
        ws.send(
          JSON.stringify({
            kind: "resize",
            cols: term.cols as number,
            rows: term.rows as number,
          }),
        );
      } catch {
        // ignore fit/resize races
      }
    });

    ws.addEventListener("message", (event) => {
      if (!term) return;
      if (typeof event.data === "string") {
        try {
          const frame = JSON.parse(event.data) as {
            kind?: string;
            detail?: string;
            status?: string;
          };
          if (frame.kind === "error") {
            setError(getErrorMessage(frame, "Terminal stream error"));
            setStatus("error");
            return;
          }
          if (frame.kind === "exit") {
            setStatus("closed");
            term.writeln("");
            term.writeln(`[process ${frame.status ?? "exited"}]`);
          }
        } catch {
          term.write(event.data);
        }
        return;
      }
      if (event.data instanceof ArrayBuffer) {
        term.write(new Uint8Array(event.data));
      }
    });

    ws.addEventListener("close", () => {
      if (disposed) return;
      if (status() === "live" || status() === "connecting") {
        setStatus("reconnecting");
      }
    });

    ws.addEventListener("error", () => {
      if (disposed) return;
      setStatus("error");
      setError("Terminal connection failed: the WebSocket closed before the terminal became live.");
    });
  }

  async function bootstrap() {
    const projectId = props.projectId();
    if (!projectId) return;
    if (isMobile()) return;
    setStatus("loading");
    setError("");
    try {
      const existing = await listTerminals(projectId);
      const live = existing.find((t) => t.status === "running" || t.status === "starting");
      if (live) {
        await connectTo(live);
        return;
      }

      const cols = Math.max(40, term?.cols ?? 80);
      const rows = Math.max(10, term?.rows ?? 24);
      const created = await createTerminal({
        project_id: projectId,
        size: { cols, rows },
        working_directory: ".",
      });
      invalidateList();
      await connectTo(created);
    } catch (err) {
      setStatus("error");
      setError(getErrorMessage(err, "Failed to open terminal"));
    }
  }

  async function onReconnect() {
    tearDownSocket();
    if (term) {
      term.reset();
    }
    await bootstrap();
  }

  async function onInterrupt() {
    const id = terminal()?.id;
    if (!id) return;
    try {
      await signalTerminal(id, "ctrl_c");
    } catch (err) {
      setError(getErrorMessage(err, "Signal failed"));
    }
  }

  async function onClose() {
    const id = terminal()?.id;
    if (!id) return;
    try {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ kind: "close" }));
      }
      await closeTerminal(id);
      tearDownSocket();
      setStatus("closed");
      invalidateList();
    } catch (err) {
      setError(getErrorMessage(err, "Close failed"));
    }
  }

  onMount(() => {
    disposed = false;
    if (!isMobile() && (props.active?.() ?? true)) {
      void bootstrap();
    }
  });

  createEffect(() => {
    // Re-bootstrap when the project identity changes.
    const id = props.projectId();
    const active = props.active?.() ?? true;
    if (!id || !active || isMobile()) return;
    if (status() === "idle") {
      void bootstrap();
    }
  });

  onCleanup(() => {
    disposed = true;
    tearDownSocket();
    resizeObserver?.disconnect();
    dataDisposable?.dispose();
    try {
      term?.dispose();
    } catch {
      // ignore
    }
    term = undefined;
    fitAddon = undefined;
  });

  return (
    <div class="terminal-panel">
      <NotificationEvent
        message={error()}
        variant="danger"
        action={{ label: "Reconnect", onClick: () => void onReconnect() }}
      />
      <div class="ide-sidebar-header terminal-panel__header">
        <span>{props.title ?? "Terminal"}</span>
        <div class="terminal-panel__actions">
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label="Reconnect terminal"
            disabled={isMobile() || status() === "loading" || status() === "connecting"}
            onClick={() => void onReconnect()}
          >
            <RefreshCw size={14} />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label="Send interrupt"
            disabled={status() !== "live"}
            onClick={() => void onInterrupt()}
          >
            <Square size={14} />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label="Close terminal"
            disabled={!terminal() || status() === "closed"}
            onClick={() => void onClose()}
          >
            <X size={14} />
          </Button>
        </div>
      </div>

      <Show
        when={!isMobile()}
        fallback={
          <EmptyState
            icon={TerminalSquare}
            title="Terminal unavailable on this screen"
            description="Main Terminal is available on desktop screens."
            class="terminal-placeholder"
          />
        }
      >
        <Show when={status() === "loading" || status() === "connecting"}>
          <div class="terminal-panel__loading" role="status" aria-label="Opening terminal">
            <Loader2 size={16} class="ui-spinner" />
            <span>Opening terminal…</span>
          </div>
        </Show>
        <div
          class="terminal-panel__host"
          ref={(el) => {
            hostEl = el;
          }}
          role="application"
          aria-label={props.title ?? "Terminal"}
          // xterm writes the live terminal into this host; tabIndex lets the
          // emulator receive keyboard focus while inside `role="application"`.
          // biome-ignore lint/a11y/noNoninteractiveTabindex: terminal host must be focusable for xterm keyboard input
          tabIndex={0}
        />
      </Show>
    </div>
  );
}
