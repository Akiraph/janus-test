import { Route, Router } from "@solidjs/router";
import { lazy } from "solid-js";
import { AppShell } from "./AppShell";

const ModelsPage = lazy(() =>
  import("../features/models/ModelsPage").then((m) => ({ default: m.ModelsPage })),
);
const SecurityPage = lazy(() =>
  import("../features/security/SecurityPage").then((m) => ({ default: m.SecurityPage })),
);
const SystemPage = lazy(() =>
  import("../features/system/SystemPage").then((m) => ({ default: m.SystemPage })),
);
const ProjectsPage = lazy(() =>
  import("../features/projects/ProjectsPage").then((m) => ({ default: m.ProjectsPage })),
);
const ProjectPage = lazy(() =>
  import("../features/projects/ProjectPage").then((m) => ({ default: m.ProjectPage })),
);

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={ProjectsPage} />
      <Route path="/projects/:id" component={ProjectPage} />
      <Route path="/settings" component={ModelsPage} />
      <Route path="/system" component={SystemPage} />
      <Route path="/models" component={ModelsPage} />
      <Route path="/security" component={SecurityPage} />
    </Router>
  );
}
