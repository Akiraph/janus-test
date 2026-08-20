import { A } from "@solidjs/router";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import Files from "lucide-solid/icons/files";
import GitCompare from "lucide-solid/icons/git-compare";
import Loader2 from "lucide-solid/icons/loader-2";
import MessageSquare from "lucide-solid/icons/message-square";
import TerminalSquare from "lucide-solid/icons/terminal-square";

export function IdeShellScaffold() {
  return (
    <section class="project-page project-page--ide" aria-busy="true">
      <header class="workspace-topbar">
        <A class="project-back" href="/">
          <ArrowLeft size={16} aria-hidden="true" />
          Exit
        </A>
        <div class="workspace-title-row">
          <div class="workspace-identity">
            <div class="workspace-name">
              <span>Workspace:</span>
              <h1 id="project-title">Loading…</h1>
            </div>
          </div>
        </div>
      </header>
      <div class="ide-shell">
        <div class="ide-activity-bar" aria-hidden="true">
          <span class="ide-activity-btn">
            <MessageSquare size={18} />
            <span>Sessions</span>
          </span>
          <span class="ide-activity-btn">
            <Files size={18} />
            <span>Explorer</span>
          </span>
          <span class="ide-activity-btn">
            <GitCompare size={18} />
            <span>Source Control</span>
          </span>
          <span class="ide-activity-btn">
            <TerminalSquare size={18} />
            <span>Terminal</span>
          </span>
        </div>
        <aside class="ide-sidebar" aria-hidden="true" />
        <div class="ide-main">
          <div class="ide-main-surface">
            <div class="ide-shell-scaffold-loading" role="status">
              <Loader2 size={22} class="ui-spinner" aria-hidden="true" />
              <span class="sr-only">Loading workspace</span>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
