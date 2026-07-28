import { Route, Router } from "@solidjs/router";
import { lazy } from "solid-js";
import { AppShell } from "./AppShell";

const ModelsPage = lazy(() =>
  import("../pages/ModelsPage").then((m) => ({ default: m.ModelsPage })),
);
const SecurityPage = lazy(() =>
  import("../pages/SecurityPage").then((m) => ({ default: m.SecurityPage })),
);
const SystemPage = lazy(() =>
  import("../pages/SystemPage").then((m) => ({ default: m.SystemPage })),
);
const ProjectsPage = lazy(() =>
  import("../pages/ProjectsPage").then((m) => ({ default: m.ProjectsPage })),
);
const ProjectPage = lazy(() =>
  import("../pages/ProjectPage").then((m) => ({ default: m.ProjectPage })),
);

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={ProjectsPage} />
      <Route path="/projects/:id" component={ProjectPage} />
      {/* Legacy session routes collapse into the project shell (tabs, not pages). */}
      <Route path="/projects/:id/sessions/*rest" component={ProjectPage} />
      <Route path="/settings" component={ModelsPage} />
      <Route path="/system" component={SystemPage} />
      <Route path="/models" component={ModelsPage} />
      <Route path="/security" component={SecurityPage} />
    </Router>
  );
}
