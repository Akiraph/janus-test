import { useMutation, useQueryClient } from "@tanstack/react-query";
import { deleteProject } from "../../../lib/api-client";

export function useDeleteProject() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (projectId: string) => deleteProject(projectId),
    onSuccess: () => {
      // Invalidate projects list
      queryClient.invalidateQueries({ queryKey: ["projects"] });
    },
  });
}
