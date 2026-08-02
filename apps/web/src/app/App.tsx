import { Route, Router, useParams } from "@solidjs/router";
import { lazy } from "solid-js";
import { AppShell } from "./AppShell";

const ModelsRoute = lazy(() =>
  import("../features/models/ModelsSettings").then((m) => ({ default: m.ModelsSettings })),
);
const SecurityRoute = lazy(() =>
  import("../features/security/SecuritySettings").then((m) => ({ default: m.SecuritySettings })),
);
const SystemRoute = lazy(() =>
  import("../features/system/SystemStatus").then((m) => ({ default: m.SystemStatus })),
);
const ProjectsRoute = lazy(() =>
  import("../features/projects/ProjectsOverview").then((m) => ({ default: m.ProjectsOverview })),
);
const ProjectWorkspaceRoute = lazy(() =>
  import("../features/projects/ProjectWorkspace").then((m) => ({ default: m.ProjectWorkspace })),
);

function ProjectRoute() {
  const params = useParams<{ id: string }>();
  return <ProjectWorkspaceRoute projectId={params.id} />;
}

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={ProjectsRoute} />
      <Route path="/projects/:id" component={ProjectRoute} />
      <Route path="/settings" component={ModelsRoute} />
      <Route path="/system" component={SystemRoute} />
      <Route path="/models" component={ModelsRoute} />
      <Route path="/security" component={SecurityRoute} />
    </Router>
  );
}
