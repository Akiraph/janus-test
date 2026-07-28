import { useQueryClient } from "@tanstack/solid-query";
import Loader2 from "lucide-solid/icons/loader-2";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Square from "lucide-solid/icons/square";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import X from "lucide-solid/icons/x";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { Button } from "../../../components/ui/Button";
import { EmptyState } from "../../../components/ui/EmptyState";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import type { TerminalOwnerInput, TerminalProjection, TerminalTicket } from "../../../lib/api";
import {
  closeTerminal,
  createTerminal,
  issueTerminalTicket,
  listTerminals,
  signalTerminal,
  terminalConnectUrl,
} from "../../../lib/api";
import { useIsMobile } from "../../../lib/viewport";

export type TerminalOwnerKind = "project" | "session";

interface TerminalPanelProps {
  /** Project id always known for Main Terminal; Session Terminal also needs session id. */
  projectId: () => string | undefined;
  ownerKind: TerminalOwnerKind;
  ownerId: () => string | undefined;
  /** Session Terminal shows a direct-write warning. */
  warnAgentCopy?: boolean;
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

/**
 * Lazy Terminal panel. The xterm emulator is dynamically imported only when this
 * component mounts so the initial web bundle stays free of the emulator weight.
 *
 * createTerminal currently requires a durable Runtime id. Public HTTP does not
 * yet expose Runtime ensure/list for Project owners, so the panel reuses an
 * existing Terminal when present and surfaces a clear recovery state when create
 * cannot proceed without a Runtime.
 */
export function TerminalPanel(props: TerminalPanelProps) {
  const queryClient = useQueryClient();
  const isMobile = useIsMobile();
  const [status, setStatus] = createSignal<ConnectState>("idle");
  const [error, setError] = createSignal("");
  const [terminal, setTerminal] = createSignal<TerminalProjection | null>(null);
  const [runtimeHint, setRuntimeHint] = createSignal("");

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

  const owner = (): TerminalOwnerInput | undefined => {
    const id = props.ownerId();
    if (!id) return undefined;
    return props.ownerKind === "project" ? { kind: "project", id } : { kind: "session", id };
  };

  function invalidateList() {
    const o = owner();
    if (!o) return;
    void queryClient.invalidateQueries({ queryKey: ["terminals", o.kind, o.id] });
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
      setError(err instanceof Error ? err.message : "Failed to issue terminal ticket");
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
            setError(frame.detail ?? "Terminal stream error");
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
      setError("Terminal connection failed");
    });
  }

  async function bootstrap() {
    const o = owner();
    if (!o) return;
    if (isMobile()) return;
    setStatus("loading");
    setError("");
    setRuntimeHint("");
    try {
      const existing = await listTerminals({ kind: o.kind, id: o.id });
      const live = existing.find((t) => t.status === "running" || t.status === "starting");
      if (live) {
        await connectTo(live);
        return;
      }

      // Prefer reusing a runtime_id from a prior terminal for this owner when
      // present; otherwise we cannot create without a public Runtime ensure API.
      const prior = existing[existing.length - 1];
      if (!prior?.runtime_id) {
        setStatus("error");
        setRuntimeHint(
          "No Runtime is attached yet. Start a Session Turn that uses process execution first, or wait until Runtime ensure is exposed over HTTP.",
        );
        setError("Terminal needs a Runtime before it can start");
        return;
      }

      const cols = Math.max(40, term?.cols ?? 80);
      const rows = Math.max(10, term?.rows ?? 24);
      const created = await createTerminal({
        runtime_id: prior.runtime_id,
        owner: o,
        size: { cols, rows },
        working_directory: ".",
      });
      invalidateList();
      await connectTo(created);
    } catch (err) {
      setStatus("error");
      setError(err instanceof Error ? err.message : "Failed to open terminal");
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
      setError(err instanceof Error ? err.message : "Signal failed");
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
      setError(err instanceof Error ? err.message : "Close failed");
    }
  }

  onMount(() => {
    disposed = false;
    if (!isMobile() && (props.active?.() ?? true)) {
      void bootstrap();
    }
  });

  createEffect(() => {
    // Re-bootstrap when the owner identity changes (project/session switch).
    const id = props.ownerId();
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
    <div class="terminal-panel" data-owner-kind={props.ownerKind}>
      <div class="ide-sidebar-header terminal-panel__header">
        <span>{props.title ?? "Terminal"}</span>
        <div class="terminal-panel__actions">
          <Show when={terminal()}>
            <Badge
              variant={
                status() === "live" ? "success" : status() === "error" ? "danger" : "warning"
              }
            >
              {status()}
            </Badge>
          </Show>
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

      <Show when={props.warnAgentCopy}>
        <p class="terminal-panel__warning" role="note">
          Session Terminal writes directly to the Agent workspace copy. Changes mark the Session
          dirty and may require a revision refresh before Apply/Sync.
        </p>
      </Show>

      <Show
        when={!isMobile()}
        fallback={
          <EmptyState
            icon={TerminalSquare}
            title="Terminal unavailable on this screen"
            description="Interactive Terminal is desktop-only. Use Job, Service, Ask, Steer, and Cancel controls in the Session document on small screens."
            class="terminal-placeholder"
          />
        }
      >
        <Show when={error()}>
          <div class="terminal-panel__error">
            <ErrorBlock message={error()} retry={() => void onReconnect()} />
            <Show when={runtimeHint()}>
              <p class="terminal-panel__hint muted">{runtimeHint()}</p>
            </Show>
          </div>
        </Show>
        <Show when={status() === "loading" || status() === "connecting"}>
          <div class="terminal-panel__loading" role="status" aria-label="Opening terminal">
            <Loader2 size={16} class="sessions-panel__spin" />
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
