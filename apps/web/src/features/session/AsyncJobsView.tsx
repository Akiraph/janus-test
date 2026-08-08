import { Briefcase, CircleAlert, Loader2, Square } from "lucide-solid";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import {
  cancelJob,
  getErrorMessage,
  getJobLog,
  type JobProjection,
  type LogRange,
} from "../../lib/api";

interface AsyncJobsViewProps {
  jobs: readonly JobProjection[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
}

export function AsyncJobsView(props: AsyncJobsViewProps) {
  return (
    <Show
      when={!props.loading || props.jobs.length > 0}
      fallback={<p class="session-async__empty">Loading async jobs...</p>}
    >
      <Show when={!props.error} fallback={<p class="session-async__empty">{props.error}</p>}>
        <Show
          when={props.jobs.length > 0}
          fallback={<p class="session-async__empty">No async jobs.</p>}
        >
          <div class="session-async__jobs">
            <For each={props.jobs}>
              {(job) => <AsyncJobCard job={job} onRefresh={props.onRefresh} />}
            </For>
          </div>
        </Show>
      </Show>
    </Show>
  );
}

function AsyncJobCard(props: { job: JobProjection; onRefresh: () => void }) {
  const [log, setLog] = createSignal<LogRange>();
  const [error, setError] = createSignal<string>();
  const [canceling, setCanceling] = createSignal(false);

  const refreshLog = async () => {
    try {
      setLog(await getJobLog(props.job.id, { limit: 512 * 1024 }));
      setError(undefined);
    } catch (value) {
      setError(getErrorMessage(value, "Output unavailable"));
    }
  };

  createEffect(() => {
    const status = props.job.status;
    void refreshLog();
    if (status === "queued" || status === "running") {
      const timer = setInterval(() => void refreshLog(), 1000);
      onCleanup(() => clearInterval(timer));
    }
  });

  async function stop() {
    setCanceling(true);
    try {
      await cancelJob(props.job.id);
      props.onRefresh();
    } catch (value) {
      setError(getErrorMessage(value, "Job could not be canceled"));
    } finally {
      setCanceling(false);
    }
  }

  const label = () =>
    props.job.cli_kind === "claude_code"
      ? "Claude Code"
      : props.job.cli_kind === "codex"
        ? "Codex"
        : "Bash";
  const output = () =>
    log()
      ?.chunks.map((chunk) => `[${chunk.channel}] ${chunk.text}`)
      .join("") ?? "";
  const active = () => props.job.status === "queued" || props.job.status === "running";

  return (
    <article class="async-job-card">
      <header class="async-job-card__head">
        <div class="async-job-card__identity">
          <Briefcase size={15} />
          <strong>{label()}</strong>
          <span class={`async-job-card__status async-job-card__status--${props.job.status}`}>
            {props.job.status}
          </span>
        </div>
        <Show when={active()}>
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label="Stop job"
            disabled={canceling()}
            onClick={() => void stop()}
          >
            <Show when={canceling()} fallback={<Square size={14} />}>
              <Loader2 size={14} class="ui-spinner" />
            </Show>
          </Button>
        </Show>
      </header>
      <p class="async-job-card__command">{props.job.command_summary}</p>
      <p class="async-job-card__id">{props.job.id}</p>
      <Show when={error()}>
        <p class="async-job-card__error">
          <CircleAlert size={14} /> {error()}
        </p>
      </Show>
      <pre class="async-job-card__output">{output() || "Waiting for output..."}</pre>
    </article>
  );
}
