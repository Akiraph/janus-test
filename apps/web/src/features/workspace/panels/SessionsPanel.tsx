import type { ThreadStatus } from "@janus/shared";
import {
  Check,
  CheckCircle2,
  Circle,
  CircleSlash,
  Loader2,
  MessageSquare,
  MoreVertical,
  Pencil,
  Plus,
  Trash2,
  X,
  XCircle,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../../components/ui/button";
import { DropdownMenu } from "../../../components/ui/dropdown-menu";
import { EmptyState } from "../../../components/ui/empty-state";
import { Input } from "../../../components/ui/input";
import { Tooltip } from "../../../components/ui/tooltip";
import { cn } from "../../../lib/cn";
import {
  useCreateSession,
  useDeleteSession,
  useProjectThreads,
  useRenameSession,
} from "../hooks";

interface SessionsPanelProps {
  projectId: string;
  selectedSessionId?: string | undefined;
  onSessionSelect: (sessionId: string) => void;
  onSessionDeleted?: ((sessionId: string) => void) | undefined;
}

export function SessionsPanel({
  projectId,
  selectedSessionId,
  onSessionSelect,
  onSessionDeleted,
}: SessionsPanelProps) {
  const { data, isLoading, isError, refetch } = useProjectThreads(projectId);
  const createSessionMutation = useCreateSession();
  const deleteSessionMutation = useDeleteSession(projectId);
  const renameSessionMutation = useRenameSession(projectId);
  const [renamingSessionId, setRenamingSessionId] = useState<
    string | undefined
  >();
  const [renameDraft, setRenameDraft] = useState("");

  const handleNewSession = async () => {
    try {
      // No alias: the server creates the session in "starting" state and waits
      // for a credential to be configured before launching the sandbox.
      const result = await createSessionMutation.mutateAsync({
        projectId,
      });
      onSessionSelect(result.session.id);
    } catch (error) {
      console.error("Failed to create session:", error);
    }
  };

  const handleDelete = async (sessionId: string) => {
    try {
      await deleteSessionMutation.mutateAsync(sessionId);
      onSessionDeleted?.(sessionId);
    } catch (error) {
      console.error("Failed to delete session:", error);
    }
  };

  const handleStartRename = (sessionId: string, title: string) => {
    setRenamingSessionId(sessionId);
    setRenameDraft(title);
  };

  const handleCancelRename = () => {
    setRenamingSessionId(undefined);
    setRenameDraft("");
  };

  const handleSubmitRename = async (
    sessionId: string,
    currentTitle: string,
  ) => {
    const title = renameDraft.trim();

    if (title.length === 0) {
      return;
    }

    if (title === currentTitle) {
      handleCancelRename();
      return;
    }

    try {
      await renameSessionMutation.mutateAsync({ sessionId, title });
      handleCancelRename();
    } catch (error) {
      console.error("Failed to rename session:", error);
    }
  };

  if (isLoading) {
    return (
      <div className="flex h-full flex-col">
        <SessionsPanelHeader onNewSession={handleNewSession} />
        <div className="flex flex-1 items-center justify-center">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
        </div>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="flex h-full flex-col">
        <SessionsPanelHeader onNewSession={handleNewSession} />
        <div className="flex flex-1 items-center justify-center p-6">
          <div className="text-center">
            <p className="text-sm text-destructive">Failed to load sessions</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetch()}
              className="mt-3"
            >
              Retry
            </Button>
          </div>
        </div>
      </div>
    );
  }

  const threads = data?.threads ?? [];

  if (threads.length === 0) {
    return (
      <div className="flex h-full flex-col">
        <SessionsPanelHeader onNewSession={handleNewSession} />
        <div className="flex flex-1 items-center justify-center p-6">
          <EmptyState
            icon={<MessageSquare className="h-12 w-12" />}
            title="No sessions yet"
            description="Create a new session to start working on this project."
            action={
              <Button
                onClick={handleNewSession}
                disabled={createSessionMutation.isPending}
                className="gap-2"
              >
                <Plus className="h-4 w-4" />
                {createSessionMutation.isPending
                  ? "Creating..."
                  : "New session"}
              </Button>
            }
          />
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <SessionsPanelHeader onNewSession={handleNewSession} />
      <div className="flex-1 overflow-y-auto">
        <div className="space-y-0.5 p-2">
          {threads.map((thread) => (
            <SessionItem
              key={thread.sessionId}
              title={thread.title}
              status={thread.status}
              isRenaming={thread.sessionId === renamingSessionId}
              renameDraft={renameDraft}
              isRenamePending={renameSessionMutation.isPending}
              onRenameDraftChange={setRenameDraft}
              isSelected={thread.sessionId === selectedSessionId}
              onSelect={() => onSessionSelect(thread.sessionId)}
              onStartRename={() =>
                handleStartRename(thread.sessionId, thread.title)
              }
              onCancelRename={handleCancelRename}
              onSubmitRename={() =>
                handleSubmitRename(thread.sessionId, thread.title)
              }
              onDelete={() => handleDelete(thread.sessionId)}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

interface SessionsPanelHeaderProps {
  onNewSession: () => void;
}

function SessionsPanelHeader({ onNewSession }: SessionsPanelHeaderProps) {
  return (
    <div className="flex h-11 shrink-0 items-center justify-between gap-2 border-b border-border px-4">
      <div className="flex min-w-0 items-center gap-2">
        <MessageSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
        <h2 className="truncate text-sm font-semibold">Sessions</h2>
      </div>
      <Button
        variant="ghost"
        size="sm"
        onClick={onNewSession}
        className="h-7 gap-1.5 px-2 text-xs"
      >
        <Plus className="h-3.5 w-3.5" />
        New session
      </Button>
    </div>
  );
}

interface SessionItemProps {
  title: string;
  status: ThreadStatus;
  isRenaming: boolean;
  renameDraft: string;
  isRenamePending: boolean;
  isSelected: boolean;
  onRenameDraftChange: (value: string) => void;
  onSelect: () => void;
  onStartRename: () => void;
  onCancelRename: () => void;
  onSubmitRename: () => void;
  onDelete: () => void;
}

function SessionItem({
  title,
  status,
  isRenaming,
  renameDraft,
  isRenamePending,
  isSelected,
  onRenameDraftChange,
  onSelect,
  onStartRename,
  onCancelRename,
  onSubmitRename,
  onDelete,
}: SessionItemProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isRenaming) {
      return;
    }

    inputRef.current?.focus();
    inputRef.current?.select();
  }, [isRenaming]);

  return (
    <div
      className={cn(
        "group flex items-center gap-2 rounded-md pl-2 pr-1 transition-colors",
        isSelected ? "bg-info-soft text-foreground" : "hover:bg-muted",
      )}
    >
      {isRenaming ? (
        <form
          className="flex min-w-0 flex-1 items-center gap-1 py-1"
          onSubmit={(event) => {
            event.preventDefault();
            onSubmitRename();
          }}
        >
          <StatusIcon status={status} />
          <Input
            ref={inputRef}
            value={renameDraft}
            onChange={(event) => onRenameDraftChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                onCancelRename();
              }
            }}
            disabled={isRenamePending}
            aria-label="Session name"
            className="h-7 min-w-0 px-2 py-1 text-sm"
          />
          <Tooltip content="Save" side="right">
            <button
              type="submit"
              aria-label="Save session name"
              disabled={isRenamePending || renameDraft.trim().length === 0}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-border-strong/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              <Check className="h-4 w-4" />
            </button>
          </Tooltip>
          <Tooltip content="Cancel" side="right">
            <button
              type="button"
              aria-label="Cancel session rename"
              disabled={isRenamePending}
              onClick={onCancelRename}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-border-strong/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            >
              <X className="h-4 w-4" />
            </button>
          </Tooltip>
        </form>
      ) : (
        <button
          type="button"
          onClick={onSelect}
          className="flex min-w-0 flex-1 items-center gap-2 py-2 text-left"
        >
          <StatusIcon status={status} />
          <span className="truncate text-sm font-medium leading-tight">
            {title}
          </span>
        </button>
      )}

      {!isRenaming && (
        <DropdownMenu
          trigger={
            <button
              type="button"
              aria-label="Session options"
              onClick={(e) => e.stopPropagation()}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground opacity-0 transition-opacity hover:bg-border-strong/50 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
            >
              <MoreVertical className="h-4 w-4" />
            </button>
          }
          items={[
            {
              id: "rename",
              label: "Rename",
              icon: <Pencil className="h-4 w-4" />,
              onClick: onStartRename,
            },
            "separator",
            {
              id: "delete",
              label: "Delete",
              icon: <Trash2 className="h-4 w-4" />,
              onClick: onDelete,
            },
          ]}
        />
      )}
    </div>
  );
}

function StatusIcon({ status }: { status: ThreadStatus }) {
  const { icon, label } = describeStatus(status);
  return (
    <Tooltip content={label} side="right">
      <span className="flex h-4 w-4 shrink-0 items-center justify-center">
        {icon}
      </span>
    </Tooltip>
  );
}

function describeStatus(status: ThreadStatus): {
  icon: React.ReactNode;
  label: string;
} {
  switch (status) {
    case "queued":
    case "running":
      return {
        icon: <Loader2 className="h-3.5 w-3.5 animate-spin text-info" />,
        label: formatStatus(status),
      };
    case "completed":
      return {
        icon: <CheckCircle2 className="h-3.5 w-3.5 text-success" />,
        label: "Completed",
      };
    case "canceled":
      return {
        icon: <CircleSlash className="h-3.5 w-3.5 text-muted-foreground" />,
        label: "Canceled",
      };
    case "failed":
      return {
        icon: <XCircle className="h-3.5 w-3.5 text-destructive" />,
        label: "Failed",
      };
    default:
      return {
        icon: <Circle className="h-3 w-3 text-muted-foreground" />,
        label: "Idle",
      };
  }
}

function formatStatus(status: ThreadStatus): string {
  switch (status) {
    case "idle":
      return "Idle";
    case "queued":
      return "Queued";
    case "running":
      return "Running";
    case "completed":
      return "Completed";
    case "canceled":
      return "Canceled";
    case "failed":
      return "Failed";
    default:
      return status;
  }
}
