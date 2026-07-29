import { A } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import Files from "lucide-solid/icons/files";
import GitCompare from "lucide-solid/icons/git-compare";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import TerminalSquare from "lucide-solid/icons/terminal-square";

/**
 * Empty-but-mounted IDE shell shown while the project page's queries are
 * suspended (SolidJS resource pending on first fetch / after a cache miss).
 *
 * This is the load-bearing fix for "the whole workspace vanishes when I click
 * a session": createQuery uses createResource, which suspends to the nearest
 * <Suspense> boundary. Without this scaffold that boundary was AppShell's, so
 * any session-switch that hit a cache miss replaced the entire ProjectPage
 * (activity rail, sidebar, tab strip — all of it) with a spinner. Putting a
 * Suspense boundary here with this scaffold as fallback keeps the workspace
 * chrome permanently visible; only the main-area surface shows a spinner.
 */
export function IdeShellScaffold() {
  return (
    <section class="project-page project-page--ide" aria-busy="true">
      <header class="workspace-topbar">
        <A class="project-back" href="/">
          <ArrowLeft size={16} />
          Exit
        </A>
        <div class="workspace-title-row">
          <div class="workspace-identity">
            <div class="workspace-name">
              <span>Workspace:</span>
              <h1 id="project-title">…</h1>
            </div>
          </div>
        </div>
      </header>
      <div class="ide-shell">
        <nav class="ide-activity-bar" aria-label="Workspace activity" aria-disabled="true">
          <span class="ide-activity-btn" aria-hidden="true">
            <MessageSquare size={18} />
            <span>Sessions</span>
          </span>
          <span class="ide-activity-btn" aria-hidden="true">
            <Files size={18} />
            <span>Explorer</span>
          </span>
          <span class="ide-activity-btn" aria-hidden="true">
            <GitCompare size={18} />
            <span>Source Control</span>
          </span>
          <span class="ide-activity-btn" aria-hidden="true">
            <TerminalSquare size={18} />
            <span>Terminal</span>
          </span>
        </nav>
        <aside class="ide-sidebar" aria-label="Workspace sidebar" aria-hidden="true" />
        <main class="ide-main">
          <div class="ide-main-surface">
            <div class="ide-shell-scaffold-loading" role="status" aria-label="Loading workspace">
              <Loader2 size={22} class="ui-spinner" />
            </div>
          </div>
        </main>
      </div>
    </section>
  );
}
