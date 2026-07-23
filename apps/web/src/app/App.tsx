import { Route, Router } from "@solidjs/router";
import { ModelsPage } from "../features/models/ModelsPage";
import { SecurityPage } from "../features/security/SecurityPage";
import { SystemPage } from "../features/system/SystemPage";
import { WorkspacePage } from "../features/workspace/WorkspacePage";
import { AppShell } from "./AppShell";

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={WorkspacePage} />
      <Route path="/system" component={SystemPage} />
      <Route path="/models" component={ModelsPage} />
      <Route path="/security" component={SecurityPage} />
    </Router>
  );
}
