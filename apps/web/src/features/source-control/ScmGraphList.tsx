import { createMemo, createSignal, For, Show } from "solid-js";
import { Alt } from "../../components/ui/Alt";
import { useGitLog } from "../../lib/queries";
import { CommitAltContent, formatCommitTime } from "./commitMeta";

interface ScmGraphListProps {
  projectId: () => string | undefined;
  branch?: (() => string | null) | undefined;
}

/**
 * Compact single-lane commit list for the Source Control side panel.
 * Visual model matches a simple history rail: one vertical line, solid dots
 * for history, a hollow ring for HEAD. Hover/focus shows a custom Alt bubble
 * with full commit details; click selects the row for highlight.
 */
export function ScmGraphList(props: ScmGraphListProps) {
  const log = useGitLog(props.projectId, 50);
  const [selected, setSelected] = createSignal<string | null>(null);
  const entries = createMemo(() => log.data ?? []);

  return (
    <section class="scm-graph-list" aria-label="Source control commit graph">
      <Show when={!log.isPending} fallback={<p class="surface-note">Loading...</p>}>
        <Show when={entries().length > 0} fallback={<p class="surface-note">No commits yet</p>}>
          <ol class="scm-graph-rows">
            <For each={entries()}>
              {(commit, index) => {
                const latest = () => index() === 0;
                const isSelected = () => selected() === commit.sha;
                return (
                  <li>
                    <Alt content={<CommitAltContent commit={commit} />} class="alt-bubble--commit">
                      <button
                        type="button"
                        class="scm-graph-row"
                        classList={{ "scm-graph-row--selected": isSelected() }}
                        aria-pressed={isSelected()}
                        onClick={() => setSelected(commit.sha)}
                      >
                        <span
                          class="scm-graph-rail"
                          classList={{
                            "scm-graph-rail--first": latest(),
                            "scm-graph-rail--last": index() === entries().length - 1,
                          }}
                          aria-hidden="true"
                        >
                          <span
                            class="scm-graph-dot"
                            classList={{ "scm-graph-dot--hollow": latest() }}
                          />
                        </span>
                        <span class="scm-graph-content">
                          <span class="scm-graph-message-row">
                            <span class="scm-graph-message">{commit.message}</span>
                            <Show when={latest() && props.branch?.()}>
                              <span class="scm-graph-ref">{props.branch?.()}</span>
                            </Show>
                          </span>
                          <span class="scm-graph-meta">
                            <code>{commit.sha.slice(0, 7)}</code>
                            <span>{commit.author}</span>
                            <span>{formatCommitTime(commit.committed_at)}</span>
                          </span>
                        </span>
                      </button>
                    </Alt>
                  </li>
                );
              }}
            </For>
          </ol>
        </Show>
      </Show>
    </section>
  );
}
