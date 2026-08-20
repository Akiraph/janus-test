import { Briefcase, CircleAlert, Loader2, Square } from "lucide-solid";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import {
  type AsyncTaskProjection,
  cancelAsyncTask,
  getAsyncTaskLog,
  getErrorMessage,
  type LogRange,
} from "../../lib/api";

interface AsyncTasksViewProps {
  tasks: readonly AsyncTaskProjection[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
}

export function AsyncTasksView(props: AsyncTasksViewProps) {
  return (
    <Show
      when={!props.loading || props.tasks.length > 0}
      fallback={
        <p class="session-async__empty" role="status">
          Loading async tasks...
        </p>
      }
    >
      <Show
        when={!props.error}
        fallback={
          <div class="session-async__error" role="alert">
            <p class="session-async__empty">{props.error}</p>
            <Button variant="outline" size="sm" onClick={props.onRefresh}>
              Retry
            </Button>
          </div>
        }
      >
        <Show
          when={props.tasks.length > 0}
          fallback={<p class="session-async__empty">No async tasks.</p>}
        >
          <div class="session-async__tasks">
            <For each={props.tasks}>
              {(task) => <AsyncTaskCard task={task} onRefresh={props.onRefresh} />}
            </For>
          </div>
        </Show>
      </Show>
    </Show>
  );
}

function AsyncTaskCard(props: { task: AsyncTaskProjection; onRefresh: () => void }) {
  const [log, setLog] = createSignal<LogRange>();
  const [error, setError] = createSignal<string>();
  const [canceling, setCanceling] = createSignal(false);

  const refreshLog = async () => {
    try {
      setLog(await getAsyncTaskLog(props.task.id, { limit: 512 * 1024 }));
      setError(undefined);
    } catch (value) {
      setError(getErrorMessage(value, "Output unavailable"));
    }
  };

  createEffect(() => {
    const status = props.task.status;
    void refreshLog();
    if (status === "queued" || status === "running") {
      const timer = setInterval(() => void refreshLog(), 1000);
      onCleanup(() => clearInterval(timer));
    }
  });

  async function stop() {
    setCanceling(true);
    try {
      await cancelAsyncTask(props.task.id);
      props.onRefresh();
    } catch (value) {
      setError(getErrorMessage(value, "Async task could not be canceled"));
    } finally {
      setCanceling(false);
    }
  }

  const label = () => "Bash";
  const output = () =>
    log()
      ?.chunks.map((chunk) => `[${chunk.channel}] ${chunk.text}`)
      .join("") ?? "";
  const active = () => props.task.status === "queued" || props.task.status === "running";

  return (
    <article class="async-task-card">
      <header class="async-task-card__head">
        <div class="async-task-card__identity">
          <Briefcase size={15} aria-hidden="true" />
          <strong>{label()}</strong>
          <span class={`async-task-card__status async-task-card__status--${props.task.status}`}>
            {props.task.status}
          </span>
        </div>
        <Show when={active()}>
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label={canceling() ? "Stopping async task" : "Stop async task"}
            disabled={canceling()}
            onClick={() => void stop()}
          >
            <Show when={canceling()} fallback={<Square size={14} aria-hidden="true" />}>
              <Loader2 size={14} class="ui-spinner" aria-hidden="true" />
            </Show>
          </Button>
        </Show>
      </header>
      <p class="async-task-card__command">{props.task.command_summary}</p>
      <p class="async-task-card__id">{props.task.id}</p>
      <Show when={error()}>
        <p class="async-task-card__error" role="alert">
          <CircleAlert size={14} aria-hidden="true" /> {error()}
        </p>
      </Show>
      <pre class="async-task-card__output">{output() || "Waiting for output..."}</pre>
    </article>
  );
}
