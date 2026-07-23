import type { UpdateOwnerPasswordRequest } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { updateOwnerPassword } from "../../../lib/api-client";

export function useUpdateOwnerPassword() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: UpdateOwnerPasswordRequest) =>
      updateOwnerPassword(request),
    onSuccess(response) {
      queryClient.setQueryData(["auth-status"], response);
    },
  });
}
