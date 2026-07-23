import {
  Check,
  ChevronLeft,
  ChevronRight,
  Copy,
  GitBranch,
  Paperclip,
  Pencil,
  X,
} from "lucide-react";
import { type ReactNode, useState } from "react";
import { Button } from "../../../components/ui/button";
import { StatusDot } from "../../../components/ui/status-dot";
import { Textarea } from "../../../components/ui/textarea";
import { Tooltip } from "../../../components/ui/tooltip";
import { formatCompletedThoughtTitle } from "../data/thought-title";
import type { ConversationItem } from "../types";
import { ActionRow, ConversationCollapsibleRow } from "./ActionRow";
import { formatBytes } from "./format-bytes";
import { SupervisorOutput } from "./SupervisorOutput";

export function ConversationItemRow({
  item,
  onSelectCliJob,
  onCopyUserMessage,
  onBranchFromRun,
  onEditFromRun,
  editingUserMessage,
  onNavigateUserMessageVersion,
}: {
  readonly item: ConversationItem;
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
  readonly onCopyUserMessage?: ((text: string) => void) | undefined;
  readonly onBranchFromRun?: ((runId: string) => void) | undefined;
  readonly onEditFromRun?:
    | ((request: { readonly runId: string; readonly text: string }) => void)
    | undefined;
  readonly editingUserMessage?:
    | {
        readonly runId: string;
        readonly text: string;
        readonly saving: boolean;
        readonly onTextChange: (text: string) => void;
        readonly onSave: () => void;
        readonly onCancel: () => void;
      }
    | undefined;
  readonly onNavigateUserMessageVersion?:
    | ((request: {
        readonly parentRunId: string;
        readonly runId: string;
      }) => void)
    | undefined;
}) {
  switch (item.kind) {
    case "user":
      return (
        <UserMessage
          item={item}
          onCopy={onCopyUserMessage}
          onBranch={onBranchFromRun}
          onEdit={onEditFromRun}
          editing={
            editingUserMessage?.runId === item.runId
              ? editingUserMessage
              : undefined
          }
          onNavigateVersion={onNavigateUserMessageVersion}
        />
      );
    case "supervisor":
      return (
        <SupervisorOutput
          text={item.text}
          {...(item.tone === undefined ? {} : { tone: item.tone })}
        />
      );
    case "thought":
      return <ThoughtOutput item={item} />;
    case "action":
      return <ActionRow action={item.action} onSelectCliJob={onSelectCliJob} />;
  }
}

function UserMessage({
  item,
  onCopy,
  onBranch,
  onEdit,
  editing,
  onNavigateVersion,
}: {
  readonly item: Extract<ConversationItem, { readonly kind: "user" }>;
  readonly onCopy?: ((text: string) => void) | undefined;
  readonly onBranch?: ((runId: string) => void) | undefined;
  readonly onEdit?:
    | ((request: { readonly runId: string; readonly text: string }) => void)
    | undefined;
  readonly editing?:
    | {
        readonly text: string;
        readonly saving: boolean;
        readonly onTextChange: (text: string) => void;
        readonly onSave: () => void;
        readonly onCancel: () => void;
      }
    | undefined;
  readonly onNavigateVersion?:
    | ((request: {
        readonly parentRunId: string;
        readonly runId: string;
      }) => void)
    | undefined;
}) {
  const versionNavigation = item.versionNavigation;
  const attachments = item.attachments ?? [];

  return (
    <div className="group flex justify-end">
      <div className="flex max-w-[85%] flex-col items-end gap-1">
        {editing === undefined ? (
          <div className="rounded-md rounded-br-xs bg-muted px-3 py-2 text-sm text-foreground shadow-card">
            <div>{item.text}</div>
            {attachments.length === 0 ? null : (
              <div className="mt-2 flex flex-wrap justify-end gap-1.5">
                {attachments.map((attachment) => (
                  <span
                    key={attachment.id}
                    className="inline-flex max-w-56 items-center gap-1.5 rounded-xs border border-border bg-background/60 px-2 py-1 text-xs text-muted-foreground"
                  >
                    <Paperclip className="h-3 w-3 shrink-0" />
                    <span className="min-w-0 truncate">{attachment.name}</span>
                    <span className="shrink-0 tabular-nums">
                      {formatBytes(attachment.sizeBytes)}
                    </span>
                  </span>
                ))}
              </div>
            )}
          </div>
        ) : (
          <form
            className="w-[min(42rem,85vw)] rounded-md rounded-br-xs border border-border-accent bg-background p-2 shadow-card"
            onSubmit={(event) => {
              event.preventDefault();
              editing.onSave();
            }}
          >
            <Textarea
              value={editing.text}
              onChange={(event) => editing.onTextChange(event.target.value)}
              autoResize
              disabled={editing.saving}
              aria-label="Edit message"
              className="min-h-28 max-h-80 bg-background"
            />
            <div className="mt-2 flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={editing.onCancel}
                disabled={editing.saving}
              >
                <X className="h-3.5 w-3.5" />
                Cancel
              </Button>
              <Button type="submit" size="sm" disabled={editing.saving}>
                <Check className="h-3.5 w-3.5" />
                Save
              </Button>
            </div>
          </form>
        )}
        <div className="flex h-7 items-center gap-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
          {versionNavigation === undefined ? null : (
            <div className="mr-1 flex items-center gap-0.5 rounded-sm border border-border bg-background px-1">
              <UserMessageAction
                label="Previous version"
                disabled={versionNavigation.previousRunId === undefined}
                onClick={() => {
                  if (versionNavigation.previousRunId !== undefined) {
                    onNavigateVersion?.({
                      parentRunId: item.runId,
                      runId: versionNavigation.previousRunId,
                    });
                  }
                }}
              >
                <ChevronLeft className="h-3.5 w-3.5" />
              </UserMessageAction>
              <span className="min-w-7 text-center text-[11px] tabular-nums">
                {versionNavigation.current}/{versionNavigation.total}
              </span>
              <UserMessageAction
                label="Next version"
                disabled={versionNavigation.nextRunId === undefined}
                onClick={() => {
                  if (versionNavigation.nextRunId !== undefined) {
                    onNavigateVersion?.({
                      parentRunId: item.runId,
                      runId: versionNavigation.nextRunId,
                    });
                  }
                }}
              >
                <ChevronRight className="h-3.5 w-3.5" />
              </UserMessageAction>
            </div>
          )}
          <UserMessageAction
            label="Edit from here"
            onClick={() => onEdit?.({ runId: item.runId, text: item.text })}
          >
            <Pencil className="h-3.5 w-3.5" />
          </UserMessageAction>
          <UserMessageAction
            label="Copy message"
            onClick={() => onCopy?.(item.text)}
          >
            <Copy className="h-3.5 w-3.5" />
          </UserMessageAction>
          <UserMessageAction
            label="Branch session"
            onClick={() => onBranch?.(item.runId)}
          >
            <GitBranch className="h-3.5 w-3.5" />
          </UserMessageAction>
        </div>
      </div>
    </div>
  );
}

function UserMessageAction({
  label,
  onClick,
  disabled,
  children,
}: {
  readonly label: string;
  readonly onClick: () => void;
  readonly disabled?: boolean;
  readonly children: ReactNode;
}) {
  return (
    <Tooltip content={label}>
      <button
        type="button"
        aria-label={label}
        onClick={onClick}
        disabled={disabled}
        className="flex h-6 w-6 items-center justify-center rounded-xs transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40"
      >
        {children}
      </button>
    </Tooltip>
  );
}

function ThoughtOutput({
  item,
}: {
  readonly item: Extract<ConversationItem, { readonly kind: "thought" }>;
}) {
  const [expanded, setExpanded] = useState(true);
  const title =
    item.status === "streaming"
      ? "Thinking"
      : formatCompletedThoughtTitle(item);

  return (
    <ConversationCollapsibleRow
      open={expanded}
      onOpenChange={setExpanded}
      row={
        <>
          <StatusDot
            tone="muted"
            pulse={item.status === "streaming"}
            className="shrink-0"
          />
          <span className="min-w-0 max-w-full truncate font-medium">
            {title}
          </span>
        </>
      }
      detail={item.text}
      detailClassName="whitespace-pre-wrap break-words text-sm italic leading-relaxed text-foreground/75"
    />
  );
}
