import { useParams } from "@solidjs/router";
import { ProjectWorkspace } from "../features/projects/ProjectWorkspace";

export function ProjectPage() {
  const params = useParams<{ id: string }>();
  return <ProjectWorkspace projectId={params.id} />;
}
