import type {
  ListSupervisorRunsResponse,
  ModelAlias,
  StartSupervisorRunInput,
  SupervisorBestOfNRequest,
  SupervisorRunAttachmentInput,
  SupervisorRunDeliveryIntent,
} from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { startSupervisorRun } from "../../../lib/api-client";
import { broadcastProjectThreadsChanged } from "../data/project-thread-broadcast";
import {
  invalidateSessionDiffQueries,
  invalidateWorkspaceContentQueries,
} from "../data/query-invalidation";
import { workspaceKeys } from "../data/query-keys";
import { upsertRun } from "../data/supervisor-run-cache";

interface StartSupervisorRunParams {
  projectId: string;
  sessionId: string;
  task: string;
  model?: ModelAlias;
  deliveryIntent?: SupervisorRunDeliveryIntent;
  discussionModelIds?: readonly string[];
  attachments?: readonly SupervisorRunAttachmentInput[];
  bestOfN?: SupervisorBestOfNRequest;
}

export function useStartSupervisorRun() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      projectId,
      sessionId,
      task,
      model,
      deliveryIntent,
      discussionModelIds,
      attachments,
      bestOfN,
    }: StartSupervisorRunParams) => {
      const request: StartSupervisorRunInput = {
        projectId,
        sessionId,
        task,
        image: "janus-cli-worker:dev",
        deliveryIntent: deliveryIntent ?? "queue",
        ...(model && {
          supervisorModel: {
            modelAlias: model,
          },
        }),
        ...(discussionModelIds === undefined || discussionModelIds.length === 0
          ? {}
          : { discussionModelIds: [...discussionModelIds] }),
        ...(attachments === undefined || attachments.length === 0
          ? {}
          : { attachments: [...attachments] }),
        ...(bestOfN === undefined ? {} : { bestOfN }),
      };

      return startSupervisorRun(request);
    },
    onSuccess: (response, variables) => {
      queryClient.setQueryData<ListSupervisorRunsResponse>(
        workspaceKeys.sessionRuns(response.run.sessionId),
        (current) => upsertRun(current, response.run),
      );
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.sessionRuns(variables.sessionId),
      });
      void queryClient.invalidateQueries({
        queryKey: workspaceKeys.projectThreads(variables.projectId),
      });
      broadcastProjectThreadsChanged(variables.projectId);

      if (response.diff !== undefined) {
        invalidateSessionDiffQueries(queryClient, variables.sessionId);
        invalidateWorkspaceContentQueries(queryClient, variables.projectId);
      }
    },
  });
}
