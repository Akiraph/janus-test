import { createEffect, createMemo, createSignal } from "solid-js";
import { Badge, type BadgeVariant } from "../../../components/ui/Badge";
import { type TabItem, Tabs } from "../../../components/ui/Tabs";
import { postSessionMessage } from "../../../lib/api";
import { useSession, useSessionDiff, useSessionTimeline } from "../../../lib/queries";
import { SessionConversation } from "./SessionConversation";
import { SessionDiffView } from "./SessionDiffView";
import { decodeSessionDiff } from "./sessionDiff";
import { decodeSessionTimeline } from "./sessionTimeline";
import "./session.css";

export type SessionSubView = "main" | "diff";

interface SessionTabViewProps {
  sessionId: () => string;
  subView: () => SessionSubView;
  onSubViewChange: (view: SessionSubView) => void;
  onTitle?: (title: string) => void;
}

export function SessionTabView(props: SessionTabViewProps) {
  const session = useSession(props.sessionId);
  const timeline = useSessionTimeline(props.sessionId);
  const diff = useSessionDiff(props.sessionId, () => props.subView() === "diff");
  const [acceptedVersion, setAcceptedVersion] = createSignal("");

  const items = createMemo(() => decodeSessionTimeline(timeline.data?.items ?? []));
  const diffFiles = createMemo(() => decodeSessionDiff(diff.data));
  const active = createMemo(
    () => session.data?.state === "active" || Boolean(session.data?.active_turn_id),
  );
  const canMessage = createMemo(() =>
    session.data ? ["ready", "active"].includes(session.data.state) : false,
  );
  const status = () => session.data?.state ?? (session.isLoading ? "loading" : "unavailable");
  const tabs = createMemo<TabItem[]>(() => [
    { value: "main", label: "Main" },
    {
      value: "diff",
      label: "Diff",
      ...(diffFiles().length > 0 ? { badge: String(diffFiles().length) } : {}),
    },
  ]);

  createEffect(() => {
    const version = session.data?.version;
    if (version) setAcceptedVersion(version);
  });

  createEffect(() => {
    const title = session.data?.title;
    if (title) props.onTitle?.(title);
  });

  async function sendMessage(content: string) {
    const version = acceptedVersion() || session.data?.version;
    if (!version) throw new Error("Session is not ready");

    const result = await postSessionMessage(props.sessionId(), {
      content,
      expected_session_version: version,
    });
    setAcceptedVersion(result.session_version);
    void session.refetch();
    void timeline.refetch();
    return { route: result.route };
  }

  const conversationError = () => {
    if (!session.isLoading && !session.data)
      return errorMessage(session.error, "Session not found");
    if (timeline.isError && items().length === 0) {
      return errorMessage(timeline.error, "Conversation failed to load");
    }
    return null;
  };

  const diffError = () => (diff.isError ? errorMessage(diff.error, "Diff failed to load") : null);

  return (
    <div class="session-doc">
      <header class="session-doc__toolbar">
        <Tabs
          value={props.subView()}
          tabs={tabs()}
          aria-label="Session views"
          class="session-doc__tabs"
          onChange={(value) => props.onSubViewChange(value as SessionSubView)}
        />
        <Badge variant={statusVariant(status())}>{status()}</Badge>
      </header>

      <div class="session-doc__view" hidden={props.subView() !== "main"}>
        <SessionConversation
          items={items()}
          loading={timeline.isLoading && items().length === 0}
          error={conversationError()}
          delivery={active() ? "queue" : "send"}
          composerDisabled={!canMessage()}
          onRetry={() => {
            void session.refetch();
            void timeline.refetch();
          }}
          onSubmit={sendMessage}
        />
      </div>

      <div class="session-doc__view" hidden={props.subView() !== "diff"}>
        <SessionDiffView
          files={diffFiles()}
          loading={diff.isLoading}
          error={diffError()}
          onRetry={() => void diff.refetch()}
        />
      </div>
    </div>
  );
}

function statusVariant(status: string): BadgeVariant {
  if (status === "ready") return "success";
  if (["error", "failed", "unavailable"].includes(status)) return "danger";
  if (["active", "loading", "creating"].includes(status)) return "warning";
  return "neutral";
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}
