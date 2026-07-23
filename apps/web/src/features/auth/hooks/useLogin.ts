import type { LoginRequest } from "@janus/shared";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { login } from "../../../lib/api-client";

export function useLogin() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: LoginRequest) => login(request),
    onSuccess(response) {
      queryClient.setQueryData(["auth-status"], response);
    },
  });
}
