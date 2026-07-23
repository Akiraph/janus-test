import type { UpdateOwnerUsernameRequest } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { updateOwnerUsername } from "../../../lib/api-client";

export function useUpdateOwnerUsername() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: UpdateOwnerUsernameRequest) =>
      updateOwnerUsername(request),
    onSuccess(response) {
      queryClient.setQueryData(["auth-status"], response);
    },
  });
}
