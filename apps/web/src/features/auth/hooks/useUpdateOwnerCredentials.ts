import type { UpdateOwnerCredentialsRequest } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { updateOwnerCredentials } from "../../../lib/api-client";

export function useUpdateOwnerCredentials() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: UpdateOwnerCredentialsRequest) =>
      updateOwnerCredentials(request),
    onSuccess(response) {
      queryClient.setQueryData(["auth-status"], response);
    },
  });
}
