import type {
  ListSupervisorRunsResponse,
  SupervisorRunRecord,
} from "@janus/shared";

export function upsertRun(
  current: ListSupervisorRunsResponse | undefined,
  run: SupervisorRunRecord,
): ListSupervisorRunsResponse {
  const runs =
    current?.runs.filter((candidate) => candidate.id !== run.id) ?? [];
  return {
    runs: [...runs, run].sort((left, right) =>
      left.startedAt === right.startedAt
        ? left.id.localeCompare(right.id)
        : left.startedAt.localeCompare(right.startedAt),
    ),
  };
}
