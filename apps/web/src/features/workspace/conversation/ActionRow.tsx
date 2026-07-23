import { ChevronRight } from "lucide-react";
import { type ReactNode, useState } from "react";
import { CliBadge } from "../../../components/ui/cli-badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "../../../components/ui/collapsible";
import { type DotTone, StatusDot } from "../../../components/ui/status-dot";
import { cn } from "../../../lib/cn";
import { formatCompletedThoughtTitle } from "../data/thought-title";
import { DiffView, FileList, RawLog } from "../evidence-views";
import type {
  ActionStatus,
  ActionView,
  ActivityDetailItem,
  ThoughtConversationItem,
} from "../types";
import { TerminalOutputSections } from "./TerminalOutputSections";

interface ActionRowProps {
  readonly action: ActionView;
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
}

export function ActionRow({ action, onSelectCliJob }: ActionRowProps) {
  const [open, setOpen] = useState(false);

  if (action.variant === "dispatch") {
    return (
      <DispatchActionRow action={action} onSelectCliJob={onSelectCliJob} />
    );
  }

  const dotTone = dotToneForStatus(action.status);
  const row = (
    <>
      <StatusDot tone={dotTone} pulse={action.status === "running"} />
      <span className="min-w-0 max-w-full truncate font-medium">
        {action.title}
      </span>
      {action.meta ? (
        <span className="shrink-0 text-2xs text-faint">{action.meta}</span>
      ) : null}
    </>
  );

  if (action.detail === undefined) {
    return (
      <div className="flex w-full items-center gap-2 px-1.5 py-1 text-left text-xs">
        {row}
        {action.cli ? <CliBadge cli={action.cli} short /> : null}
      </div>
    );
  }

  return (
    <ConversationCollapsibleRow
      open={open}
      onOpenChange={setOpen}
      row={
        <>
          {row}
          {action.cli ? <CliBadge cli={action.cli} short /> : null}
        </>
      }
      detail={
        <ActionDetailBody action={action} onSelectCliJob={onSelectCliJob} />
      }
    />
  );
}

export function ConversationCollapsibleRow({
  open,
  onOpenChange,
  row,
  detail,
  detailClassName,
}: {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly row: ReactNode;
  readonly detail: ReactNode;
  readonly detailClassName?: string | undefined;
}) {
  return (
    <Collapsible open={open} onOpenChange={onOpenChange}>
      <CollapsibleTrigger
        className={cn(
          "group flex w-full items-center gap-2 px-1.5 py-1 text-left text-xs transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border-accent",
          open && "text-foreground",
        )}
      >
        {row}
        <ChevronRight
          size={13}
          aria-hidden
          className={cn(
            "shrink-0 text-faint opacity-0 transition-all group-hover:opacity-100 group-focus-visible:opacity-100",
            open && "rotate-90 opacity-100",
          )}
        />
      </CollapsibleTrigger>
      <CollapsibleContent className="overflow-hidden data-[state=closed]:animate-collapsible-up data-[state=open]:animate-collapsible-down">
        <div
          className={cn(
            "ml-2 border-l border-border-accent-soft bg-muted/35 py-2 pl-3 pr-2",
            detailClassName,
          )}
        >
          {detail}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}

function DispatchActionRow({
  action,
  onSelectCliJob,
}: {
  readonly action: ActionView;
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
}) {
  const dotTone = action.status === "failure" ? "danger" : "success";
  const label = action.meta ?? action.title;
  const detail = action.meta === undefined ? undefined : action.title;
  const content = (
    <>
      <StatusDot tone={dotTone} />
      <span className="min-w-0 truncate font-medium">{label}</span>
      {detail ? (
        <span className="min-w-0 flex-1 truncate text-muted-foreground">
          {detail}
        </span>
      ) : null}
      {action.cli ? <CliBadge cli={action.cli} short /> : null}
      {action.cliJobId !== undefined ? (
        <ChevronRight
          size={13}
          aria-hidden
          className="shrink-0 text-faint opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
        />
      ) : null}
    </>
  );

  if (action.cliJobId === undefined || onSelectCliJob === undefined) {
    return (
      <div className="flex w-full items-center gap-2 px-1.5 py-1 text-left text-xs">
        {content}
      </div>
    );
  }

  return (
    <button
      type="button"
      onClick={() => onSelectCliJob(action.cliJobId ?? "")}
      className="group flex w-full items-center gap-2 px-1.5 py-1 text-left text-xs transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border-accent"
    >
      {content}
    </button>
  );
}

function ActionDetailBody({
  action,
  onSelectCliJob,
}: {
  readonly action: ActionView;
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
}) {
  const { detail } = action;
  if (detail === undefined) {
    return null;
  }

  switch (detail.kind) {
    case "raw":
      return <RawLog lines={detail.lines} />;
    case "actions":
      return (
        <ActionGroup actions={detail.actions} onSelectCliJob={onSelectCliJob} />
      );
    case "activity":
      return (
        <ActivityGroup items={detail.items} onSelectCliJob={onSelectCliJob} />
      );
    case "terminalOutput":
      return <TerminalOutputSections output={detail.output} />;
    case "diff":
      return (
        <DiffView
          diff={detail.diff}
          {...(detail.path === undefined ? {} : { path: detail.path })}
        />
      );
    case "files":
      return <FileList paths={detail.paths} />;
  }
}

function ActionGroup({
  actions,
  onSelectCliJob,
}: {
  readonly actions: readonly ActionView[];
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
}) {
  return (
    <div className="space-y-1">
      {actions.map((action) => (
        <div key={action.id} className="text-muted-foreground">
          <ActionRow action={action} onSelectCliJob={onSelectCliJob} />
        </div>
      ))}
    </div>
  );
}

function ActivityGroup({
  items,
  onSelectCliJob,
}: {
  readonly items: readonly ActivityDetailItem[];
  readonly onSelectCliJob?: ((jobId: string) => void) | undefined;
}) {
  return (
    <div className="space-y-2">
      {items.map((item) =>
        item.kind === "thought" ? (
          <ThoughtDetail key={item.item.id} item={item.item} />
        ) : (
          <div key={item.action.id} className="text-muted-foreground">
            <ActionRow action={item.action} onSelectCliJob={onSelectCliJob} />
          </div>
        ),
      )}
    </div>
  );
}

function ThoughtDetail({ item }: { readonly item: ThoughtConversationItem }) {
  const title =
    item.status === "streaming"
      ? "Thinking"
      : formatCompletedThoughtTitle(item);

  return (
    <div className="space-y-1 text-xs">
      <div className="flex items-center gap-2 px-1.5 py-1">
        <StatusDot
          tone="muted"
          pulse={item.status === "streaming"}
          className="shrink-0"
        />
        <span className="min-w-0 truncate font-medium">{title}</span>
      </div>
      <div className="ml-2 whitespace-pre-wrap break-words border-l border-border-accent-soft bg-muted/35 py-2 pl-3 pr-2 text-sm italic leading-relaxed text-foreground/75">
        {item.text}
      </div>
    </div>
  );
}

function dotToneForStatus(status: ActionStatus): DotTone {
  switch (status) {
    case "success":
      return "success";
    case "failure":
      return "danger";
    case "running":
      return "muted";
  }
}
