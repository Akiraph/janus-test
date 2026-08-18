import { Route, Router, useParams } from "@solidjs/router";
import { lazy } from "solid-js";
import { ProjectWorkspace } from "../features/projects/ProjectWorkspace";
import { AppShell } from "./AppShell";

const ModelsRoute = lazy(() =>
  import("../features/models/ModelsSettings").then((m) => ({ default: m.ModelsSettings })),
);
const SecurityRoute = lazy(() =>
  import("../features/security/SecuritySettings").then((m) => ({ default: m.SecuritySettings })),
);
const NotificationsRoute = lazy(() =>
  import("../features/notifications/NotificationsSettings").then((m) => ({
    default: m.NotificationsSettings,
  })),
);
const AutomationRoute = lazy(() =>
  import("../features/automation/AutomationSettings").then((m) => ({
    default: m.AutomationSettings,
  })),
);
const SystemRoute = lazy(() =>
  import("../features/system/SystemStatus").then((m) => ({ default: m.SystemStatus })),
);
const ProjectsRoute = lazy(() =>
  import("../features/projects/ProjectsOverview").then((m) => ({ default: m.ProjectsOverview })),
);
function ProjectRoute() {
  const params = useParams<{ id: string }>();
  return <ProjectWorkspace projectId={params.id} />;
}

function ProjectSessionRoute() {
  const params = useParams<{ id: string; sessionId: string }>();
  return <ProjectWorkspace projectId={params.id} sessionId={params.sessionId} />;
}

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={ProjectsRoute} />
      <Route path="/projects/:id/sessions/:sessionId" component={ProjectSessionRoute} />
      <Route path="/projects/:id" component={ProjectRoute} />
      <Route path="/settings" component={ModelsRoute} />
      <Route path="/system" component={SystemRoute} />
      <Route path="/models" component={ModelsRoute} />
      <Route path="/security" component={SecurityRoute} />
      <Route path="/notifications" component={NotificationsRoute} />
      <Route path="/automation" component={AutomationRoute} />
    </Router>
  );
}
