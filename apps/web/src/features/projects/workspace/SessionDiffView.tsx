import { createSignal, For, Show } from "solid-js";
import { NotificationEvent } from "../../../components/ui/notifications";
import type { SessionDiffFile } from "./sessionDiff";

interface SessionDiffViewProps {
  files: readonly SessionDiffFile[];
  loading: boolean;
  error: string | null;
  actionError: string | null;
  onRetry: () => void;
}

export function SessionDiffView(props: SessionDiffViewProps) {
  return (
    <section class="session-diff" aria-label="Session changes">
      <NotificationEvent
        message={props.error ?? props.actionError}
        variant="danger"
        action={{ label: "Retry", onClick: props.onRetry }}
      />
      <p class="session-diff__summary">Changes against main</p>
      <Show when={!props.loading} fallback={<p class="muted">Loading changes...</p>}>
        <Show when={props.files.length > 0} fallback={<p class="muted">No changes</p>}>
          <ul class="session-diff__files">
            <For each={props.files}>{(file) => <SessionDiffFileRow file={file} />}</For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}

function SessionDiffFileRow(props: { file: SessionDiffFile }) {
  const [open, setOpen] = createSignal(false);
  const hasLines = () => props.file.hunks.some((hunk) => hunk.lines.length > 0);

  return (
    <li class="session-diff__file" classList={{ "session-diff__file--open": open() }}>
      <button
        type="button"
        class="session-diff__file-head"
        aria-expanded={open()}
        onClick={() => setOpen((value) => !value)}
      >
        <span class="session-diff__file-toggle" aria-hidden="true">
          {open() ? "-" : "+"}
        </span>
        <span class="mono session-diff__file-path">{props.file.path}</span>
        <span class="session-diff__file-stats">
          <span class="session-diff__file-additions">+{props.file.additions}</span>
          <span class="session-diff__file-deletions">-{props.file.deletions}</span>
        </span>
        <span class="session-diff__file-kind" data-kind={props.file.kind}>
          {props.file.kind}
        </span>
      </button>
      <Show when={open()}>
        <Show
          when={!props.file.binary && hasLines()}
          fallback={
            <p class="session-diff__file-empty muted">
              {props.file.binary ? "Binary preview unavailable" : "No line changes"}
            </p>
          }
        >
          <div class="session-diff__hunks">
            <For each={props.file.hunks}>
              {(hunk) => (
                <div class="session-diff__hunk">
                  <For each={hunk.lines}>
                    {(line) => (
                      <div class="session-diff__line" data-kind={line.kind}>
                        <span class="session-diff__line-number">
                          {line.kind === "skip"
                            ? ""
                            : `${line.oldNumber ?? ""}${line.oldNumber || line.newNumber ? " " : ""}${line.newNumber ?? ""}`}
                        </span>
                        <span class="session-diff__line-mark" aria-hidden="true">
                          {line.kind === "add"
                            ? "+"
                            : line.kind === "delete"
                              ? "-"
                              : line.kind === "skip"
                                ? "..."
                                : " "}
                        </span>
                        <span class="session-diff__line-text mono">{line.text}</span>
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
