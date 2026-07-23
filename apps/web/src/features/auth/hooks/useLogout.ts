import { useMutation, useQueryClient } from "@tanstack/react-query";
import { logout } from "../../../lib/api-client";

export function useLogout() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: logout,
    onSuccess(response) {
      queryClient.setQueryData(["auth-status"], response);
    },
  });
}
