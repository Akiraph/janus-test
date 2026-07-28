import GitCompare from "lucide-solid/icons/git-compare";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import Send from "lucide-solid/icons/send";
import TerminalSquare from "lucide-solid/icons/terminal-square";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { Badge } from "../../../components/ui/Badge";
import { Button } from "../../../components/ui/Button";
import { EmptyState } from "../../../components/ui/EmptyState";
import { ErrorBlock } from "../../../components/ui/ErrorBlock";
import { Select } from "../../../components/ui/Select";
import type { SessionSummary } from "../../../lib/api";
import { postSessionMessage } from "../../../lib/api";
import { useProviders, useSession, useSessionDiff, useSessionTimeline } from "../../../lib/queries";
import { useIsMobile } from "../../../lib/viewport";
import { LazyTerminalPanel } from "./LazyTerminalPanel";
import { ContextCompactPanel, renderSpecializedCard } from "./SessionCards";

export type SessionSubView = "main" | "diff";

/** Reasoning effort tiers offered in the composer. M3 carries these as local
 *  UI state only — the message API does not yet accept them, so they do not
 *  reach the supervisor until the backend contract grows the fields. */
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "xhigh" | "max";

const REASONING_EFFORTS: ReadonlyArray<{ value: ReasoningEffort; label: string }> = [
  { value: "none", label: "None" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "XHigh" },
  { value: "max", label: "Max" },
];

/** Fallback shape used while the session detail is still on the wire. The whole
 *  shell (subtabs, composer, selectors) renders against this so opening a tab
 *  never flashes a loader — only the timeline content area reflects loading. */
const EMPTY_SESSION: SessionSummary = {
  id: "",
  project_id: "",
  kind: "regular",
  state: "ready",
  version: "",
  workspace_handle: "",
  source_main_revision_id: "",
  created_at: "",
  updated_at: "",
  last_activity_at: "",
};

interface SessionTabViewProps {
  projectId: () => string | undefined;
  sessionId: () => string;
  /** Controlled sub-view (Main / Diff). Parent owns it so it survives remounts. */
  subView: () => SessionSubView;
  onSubViewChange: (view: SessionSubView) => void;
  onTitle?: (title: string) => void;
}

/**
 * Session document for the project tab strip.
 * Conversation chrome mirrors the legacy Janus ConversationView:
 * right-aligned user bubbles, left supervisor text, bottom composer shell.
 */
export function SessionTabView(props: SessionTabViewProps) {
  const session = useSession(props.sessionId);
  const timeline = useSessionTimeline(props.sessionId);
  // Diff walks the full session/main Merkle trees and can take tens of seconds
  // on large repos. Never fetch it just because the Main tab opened — only when
  // the user actually switches to the Diff sub-view.
  const diff = useSessionDiff(props.sessionId, () => props.subView() === "diff");
  const providers = useProviders();

  const [draft, setDraft] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [sendError, setSendError] = createSignal("");
  // Composer model + reasoning selectors. M3 keeps them as local UI state —
  // these do NOT travel on postSessionMessage yet; the backend message contract
  // has no model_id / reasoning_effort field. They are a visual placeholder so
  // the composer matches the legacy shape, wired when the contract grows.
  const [modelId, setModelId] = createSignal("");
  const [reasoning, setReasoning] = createSignal<ReasoningEffort>("medium");
  // Desktop-only secondary surfaces, opened on demand.
  // Context/Compact: focused panel over the timeline (no public Compact HTTP
  //   yet — the panel renders an honest empty state until the supervisor
  //   publishes context/compact timeline projections, see ContextCompactPanel).
  // Terminal: a separate fully-hidden pane; the emulator only mounts while this
  //   pane is active and is excluded on small screens via LazyTerminalPanel.
  const [contextOpen, setContextOpen] = createSignal(false);
  const [terminalOpen, setTerminalOpen] = createSignal(false);
  const isMobile = useIsMobile();
  let scroller: HTMLDivElement | undefined;
  let textareaEl: HTMLTextAreaElement | undefined;

  const items = createMemo(() => timeline.data?.items ?? []);
  const busy = createMemo(
    () => session.data?.state === "active" || Boolean(session.data?.active_turn_id),
  );
  const diffFiles = createMemo(() => {
    // Backend DiffSummary uses `paths` (not `files`); accept both so older
    // shapes and the live API both render.
    const summary = (
      diff.data as { summary?: { files?: unknown[]; paths?: unknown[] } } | undefined
    )?.summary;
    return (
      (summary?.paths as Array<Record<string, unknown>> | undefined) ??
      (summary?.files as Array<Record<string, unknown>> | undefined) ??
      []
    );
  });

  /** Flatten enabled providers' enabled models into {value: upstream_model_id,
   *  label: display_name} picklist options. */
  const modelOptions = createMemo(() => {
    const list = (providers.data ?? []).flatMap((provider) =>
      (provider.models ?? [])
        .filter((model) => model.enabled)
        .map((model) => ({ value: model.upstream_model_id, label: model.display_name })),
    );
    return list;
  });

  // Keep the composer's selected model in sync with available options: default
  // to the first model when none is chosen yet, or reset when the previous pick
  // disappeared from the list (e.g. provider disabled).
  createEffect(() => {
    const options = modelOptions();
    if (options.length === 0) {
      if (modelId()) setModelId("");
      return;
    }
    if (!options.some((option) => option.value === modelId())) {
      const first = options[0];
      if (first) setModelId(first.value);
    }
  });

  createEffect(() => {
    const title = session.data?.title;
    if (title) props.onTitle?.(title);
  });

  createEffect(() => {
    const count = items().length;
    if (count > 0 && scroller && props.subView() === "main") {
      scroller.scrollTop = scroller.scrollHeight;
    }
  });

  function resizeComposer() {
    const el = textareaEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }

  async function onSend() {
    const id = props.sessionId();
    const content = draft().trim();
    const version = session.data?.version;
    if (!id || !content || !version || sending() || busy()) return;
    setSending(true);
    setSendError("");
    try {
      await postSessionMessage(id, {
        content,
        expected_session_version: version,
      });
      setDraft("");
      if (textareaEl) {
        textareaEl.style.height = "auto";
      }
      void session.refetch();
      void timeline.refetch();
    } catch (error) {
      setSendError(error instanceof Error ? error.message : "Send failed");
    } finally {
      setSending(false);
    }
  }

  // Single render path. While the session detail is on the wire we render the
  // full shell against EMPTY_SESSION so the document is never covered by a
  // loader — only the timeline content area reflects loading / missing data.
  // There is no fallback branch, no splash, no skeleton screen.
  const sess = () => session.data ?? EMPTY_SESSION;
  const shellError = () => (!session.isLoading && !session.data ? (session.error ?? null) : null);
  const timelineLoading = () => timeline.isLoading && items().length === 0;

  return (
    <div class="session-doc">
      <div class="session-doc__subtabs" role="tablist" aria-label="Session views">
        <button
          type="button"
          role="tab"
          class="session-doc__subtab"
          classList={{ "session-doc__subtab--active": props.subView() === "main" }}
          aria-selected={props.subView() === "main"}
          onClick={() => props.onSubViewChange("main")}
        >
          <MessageSquare size={14} />
          Main
        </button>
        <button
          type="button"
          role="tab"
          class="session-doc__subtab"
          classList={{ "session-doc__subtab--active": props.subView() === "diff" }}
          aria-selected={props.subView() === "diff"}
          onClick={() => props.onSubViewChange("diff")}
        >
          <GitCompare size={14} />
          Diff
          <Show when={diffFiles().length > 0}>
            <span class="session-doc__subtab-count">{diffFiles().length}</span>
          </Show>
        </button>
        {/* Desktop-only Session Terminal sub-tab. The emulator mounts lazily and
            is entirely omitted on small screens (the panel renders an
            intentional unavailable state instead). */}
        <Show when={!isMobile()}>
          <button
            type="button"
            role="tab"
            class="session-doc__subtab"
            classList={{ "session-doc__subtab--active": terminalOpen() }}
            aria-selected={terminalOpen()}
            onClick={() => setTerminalOpen((v) => !v)}
          >
            <TerminalSquare size={14} />
            Terminal
          </button>
        </Show>
        <div class="session-doc__subtabs-spacer" />
        {/* Context / Compact panel toggle — works on every viewport because it
            only surfaces projected state, never the emulator. */}
        <Button
          variant={contextOpen() ? "outline" : "ghost"}
          size="sm"
          aria-pressed={contextOpen()}
          aria-label="Toggle context and Compact panel"
          onClick={() => setContextOpen((v) => !v)}
        >
          Context
        </Button>
        <Badge
          variant={
            sess().state === "active" ? "warning" : sess().state === "ready" ? "success" : "warning"
          }
        >
          {sess().state}
        </Badge>
      </div>

      <div
        class="session-doc__main"
        classList={{ "session-doc__pane--hidden": props.subView() !== "main" }}
      >
        <div class="session-doc__timeline" ref={scroller} role="log" aria-live="polite">
          <Show
            when={shellError()}
            fallback={
              <Show
                when={items().length > 0}
                fallback={
                  <Show
                    when={timelineLoading()}
                    fallback={
                      <EmptyState
                        icon={MessageSquare}
                        title="Start the conversation"
                        description="Send a message to run a Supervisor Turn on this Session copy."
                      />
                    }
                  >
                    <div
                      class="session-doc__inline-loading"
                      role="status"
                      aria-label="Loading timeline"
                    >
                      <Loader2 size={16} class="sessions-panel__spin" />
                    </div>
                  </Show>
                }
              >
                <For each={items()}>
                  {(item) => <TimelineCard kind={item.kind} projection={item.projection} />}
                </For>
              </Show>
            }
          >
            <ErrorBlock
              message={
                shellError() instanceof Error
                  ? (shellError() as Error).message
                  : "Session not found"
              }
            />
          </Show>
        </div>

        {/* Context / Compact panel — overlay alongside the timeline when open.
            Surfaces an honest empty state because no public Compact HTTP route
            exists yet; once the supervisor publishes context/compact timeline
            projections the panel reads them via ContextCompactPanel. */}
        <ContextCompactPanel
          items={items()}
          open={contextOpen() && props.subView() === "main"}
          onClose={() => setContextOpen(false)}
        />

        <form
          class="session-composer"
          onSubmit={(event) => {
            event.preventDefault();
            void onSend();
          }}
        >
          <Show when={sendError()}>
            <div class="session-composer__error">
              <ErrorBlock message={sendError()} />
            </div>
          </Show>
          <textarea
            ref={(el) => {
              textareaEl = el;
            }}
            class="session-composer__input"
            rows={1}
            placeholder={busy() ? "Wait for the active Turn to finish…" : "Send a message..."}
            value={draft()}
            disabled={sending() || busy()}
            onInput={(event) => {
              setDraft(event.currentTarget.value);
              resizeComposer();
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                event.preventDefault();
                void onSend();
              }
            }}
          />
          <div class="session-composer__bar">
            <div class="session-composer__controls">
              <Show
                when={modelOptions().length > 0}
                fallback={
                  <span class="session-composer__model-empty" title="No model providers configured">
                    No models
                  </span>
                }
              >
                <Select
                  class="session-composer__select"
                  aria-label="Model"
                  value={modelId()}
                  options={modelOptions()}
                  onChange={setModelId}
                />
              </Show>
              <Select
                class="session-composer__select"
                aria-label="Reasoning effort"
                value={reasoning()}
                options={REASONING_EFFORTS}
                onChange={(value) => setReasoning(value as ReasoningEffort)}
              />
            </div>
            <Button
              type="submit"
              variant="primary"
              size="sm"
              iconOnly
              disabled={sending() || busy() || !draft().trim()}
              aria-label={sending() ? "Sending" : "Send message"}
            >
              <Show when={sending()} fallback={<Send size={16} />}>
                <Loader2 size={16} class="sessions-panel__spin" />
              </Show>
            </Button>
          </div>
        </form>
      </div>

      {/* Session Terminal pane — fully distinct from Main/Diff. Desktop-only:
          LazyTerminalPanel renders an intentional unavailable state on small
          screens, so the pane itself stays mountable on every viewport. The
          emulator only loads while this pane is visible (active=terminalOpen). */}
      <div
        class="session-doc__terminal"
        classList={{ "session-doc__pane--hidden": !terminalOpen() }}
      >
        <LazyTerminalPanel
          projectId={props.projectId}
          ownerKind="session"
          ownerId={props.sessionId}
          warnAgentCopy
          active={terminalOpen}
          title="Session Terminal"
        />
      </div>

      <div
        class="session-doc__diff"
        classList={{ "session-doc__pane--hidden": props.subView() !== "diff" }}
      >
        <p class="session-doc__diff-note">
          Reviewing changes against the project main branch. Applying and syncing changes becomes
          available once collaboration controls ship.
        </p>
        <div class="session-doc__diff-badges">
          <Badge>Apply disabled</Badge>
          <Badge>Sync disabled</Badge>
        </div>
        <Show when={!diff.isLoading} fallback={<p class="muted">Loading…</p>}>
          <Show
            when={diffFiles().length > 0}
            fallback={<p class="muted">No file changes vs Main.</p>}
          >
            <ul class="session-doc__diff-files">
              <For each={diffFiles()}>{(file) => <DiffFileRow file={file} />}</For>
            </ul>
          </Show>
        </Show>
      </div>
    </div>
  );
}

type DiffLine = {
  kind?: string;
  old_no?: number | null;
  new_no?: number | null;
  text?: string;
};
type DiffHunk = { lines?: DiffLine[] };

/** One path row: collapsed by default; click expands line-level hunks. */
function DiffFileRow(props: { file: Record<string, unknown> }) {
  const [open, setOpen] = createSignal(false);
  const path = () => String(props.file.path ?? props.file.rel_path ?? "?");
  const kind = () => String(props.file.kind ?? props.file.change ?? "");
  const binary = () => Boolean(props.file.binary);
  const hunks = () => (props.file.hunks as DiffHunk[] | undefined) ?? [];
  const hasLines = () => hunks().some((h) => (h.lines?.length ?? 0) > 0);

  return (
    <li class="session-doc__diff-file" classList={{ "session-doc__diff-file--open": open() }}>
      <button
        type="button"
        class="session-doc__diff-file-head"
        aria-expanded={open()}
        onClick={() => setOpen((v) => !v)}
      >
        <span class="session-doc__diff-file-toggle" aria-hidden="true">
          {open() ? "▾" : "▸"}
        </span>
        <span class="mono session-doc__diff-file-path">{path()}</span>
        <span
          class="session-doc__diff-file-kind"
          classList={{
            "session-doc__diff-file-kind--added": kind() === "added",
            "session-doc__diff-file-kind--modified": kind() === "modified",
            "session-doc__diff-file-kind--deleted": kind() === "deleted",
          }}
        >
          {kind()}
        </span>
      </button>
      <Show when={open()}>
        <Show
          when={!binary() && hasLines()}
          fallback={
            <p class="session-doc__diff-file-empty muted">
              {binary() ? "Binary or too large to preview." : "No line-level changes."}
            </p>
          }
        >
          <div class="session-doc__diff-hunks">
            <For each={hunks()}>
              {(hunk) => (
                <div class="session-doc__diff-hunk">
                  <For each={hunk.lines ?? []}>
                    {(line) => (
                      <div
                        class="session-doc__diff-line"
                        classList={{
                          "session-doc__diff-line--add": line.kind === "add",
                          "session-doc__diff-line--delete": line.kind === "delete",
                          "session-doc__diff-line--context": line.kind === "context",
                          "session-doc__diff-line--skip": line.kind === "skip",
                        }}
                      >
                        <span class="session-doc__diff-line-no">
                          {line.kind === "skip"
                            ? ""
                            : `${line.old_no ?? ""}${line.old_no || line.new_no ? " " : ""}${line.new_no ?? ""}`}
                        </span>
                        <span class="session-doc__diff-line-mark" aria-hidden="true">
                          {line.kind === "add"
                            ? "+"
                            : line.kind === "delete"
                              ? "−"
                              : line.kind === "skip"
                                ? "⋯"
                                : " "}
                        </span>
                        <span class="session-doc__diff-line-text mono">{line.text ?? ""}</span>
                      </div>
                    )}
                  </For>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>
    </li>
  );
}

function TimelineCard(props: { kind: string; projection: unknown }) {
  const projection = () =>
    (props.projection ?? {}) as {
      text?: string;
      tool_name?: string;
      status?: string;
      summary?: unknown;
    };

  // Legacy user bubble: right-aligned muted pill, body only — no "You" header.
  if (props.kind === "user_message") {
    return (
      <div class="session-msg session-msg--user">
        <div class="session-msg__bubble">{projection().text ?? ""}</div>
      </div>
    );
  }

  // Assistant output begins as left-rail text; if this item carries one of
  // the M4 specialized cards (plan / ask / model / job / service), render that
  // instead. Specialized detection keys off the timeline `kind` and, for the
  // `tool_call` kind, the `tool_name` field the supervisor stamps on the
  // projection. Unknown tool calls fall back to the generic tool block below.
  if (props.kind === "assistant_message") {
    return (
      <div class="session-msg session-msg--assistant">
        <span class="session-msg__dot" aria-hidden="true" />
        <div class="session-msg__body">{projection().text ?? ""}</div>
      </div>
    );
  }

  if (props.kind === "tool_call") {
    const specialized = renderSpecializedCard({
      kind: props.kind,
      projection: props.projection,
    });
    if (specialized) return specialized;
    return (
      <div class="session-msg session-msg--tool">
        <header>
          <code>{projection().tool_name}</code>
          <Badge>{projection().status ?? "unknown"}</Badge>
        </header>
        <pre>{JSON.stringify(projection().summary ?? {}, null, 2)}</pre>
      </div>
    );
  }

  // Steer is a durable control the user injected mid-Turn; render it as a
  // quiet, attributed line so the timeline shows it without restoring a
  // dedicated control surface (steer HTTP is not yet public).
  if (props.kind === "steer") {
    const steer = () => (props.projection ?? {}) as { kind?: string; text?: string };
    return (
      <div class="session-msg session-msg--steer" role="note" aria-label="Steer">
        <span class="session-msg__dot" aria-hidden="true" />
        <div class="session-msg__body">
          <span class="muted">steer · </span>
          {steer().text ?? ""}
        </div>
      </div>
    );
  }

  return (
    <div class="session-msg session-msg--tool">
      <header>{props.kind}</header>
      <pre>{JSON.stringify(props.projection, null, 2)}</pre>
    </div>
  );
}
