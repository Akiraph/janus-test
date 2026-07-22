import { Route, Router } from "@solidjs/router";
import { SystemPage } from "../features/system/SystemPage";
import { WorkspacePage } from "../features/workspace/WorkspacePage";
import { AppShell } from "./AppShell";

export function App() {
  return (
    <Router root={AppShell}>
      <Route path="/" component={WorkspacePage} />
      <Route path="/system" component={SystemPage} />
    </Router>
  );
}
