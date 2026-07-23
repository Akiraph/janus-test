import type {
  ListProjectThreadsResponse,
  SessionRecord,
  SupervisorRunRecord,
  ThreadStatus,
  ThreadSummary,
} from "@janus/shared";

export function threadFromSession(
  session: SessionRecord,
  options: {
    readonly status?: ThreadStatus | undefined;
    readonly runCount?: number | undefined;
    readonly updatedAt?: string | undefined;
  } = {},
): ThreadSummary {
  return {
    sessionId: session.id,
    projectId: session.projectId,
    cli: session.cli,
    title: session.title ?? "New session",
    status: options.status ?? "idle",
    runCount: options.runCount ?? 0,
    updatedAt: options.updatedAt ?? session.startedAt,
  };
}

export function upsertThread(
  current: ListProjectThreadsResponse | undefined,
  thread: ThreadSummary,
): ListProjectThreadsResponse {
  const threads =
    current?.threads.filter((item) => item.sessionId !== thread.sessionId) ??
    [];
  return { threads: sortThreads([thread, ...threads]) };
}

export function renameThread(
  current: ListProjectThreadsResponse | undefined,
  sessionId: string,
  title: string,
): ListProjectThreadsResponse | undefined {
  if (current === undefined) {
    return current;
  }

  return {
    threads: current.threads.map((thread) =>
      thread.sessionId === sessionId ? { ...thread, title } : thread,
    ),
  };
}

export function updateThreadFromRun(
  current: ListProjectThreadsResponse | undefined,
  run: SupervisorRunRecord,
): ListProjectThreadsResponse | undefined {
  if (current === undefined) {
    return current;
  }

  let updated = false;
  const threads = current.threads.map((thread) => {
    if (thread.sessionId !== run.sessionId) {
      return thread;
    }

    updated = true;
    return {
      ...thread,
      status: run.status,
      updatedAt: run.updatedAt,
    };
  });

  return updated ? { threads: sortThreads(threads) } : current;
}

export function removeThread(
  current: ListProjectThreadsResponse | undefined,
  sessionId: string,
): ListProjectThreadsResponse | undefined {
  if (current === undefined) {
    return current;
  }

  return {
    threads: current.threads.filter((thread) => thread.sessionId !== sessionId),
  };
}

function sortThreads(threads: readonly ThreadSummary[]): ThreadSummary[] {
  return [...threads].sort((left, right) =>
    right.updatedAt === left.updatedAt
      ? left.sessionId.localeCompare(right.sessionId)
      : right.updatedAt.localeCompare(left.updatedAt),
  );
}
