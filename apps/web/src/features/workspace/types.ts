import type {
  CliKind,
  GroupDiscussionDepth,
  GroupDiscussionParticipantStatus,
  GroupDiscussionStatus,
  SessionDiff,
  SupervisorCliJobStatus,
  SupervisorRunAttachmentRecord,
} from "@janus/shared";

export interface TerminalOutputViewModel {
  readonly exitCode?: number;
  readonly stdout?: string;
  readonly stderr?: string;
  readonly stdoutTruncated?: boolean;
  readonly stderrTruncated?: boolean;
}

export type ActivityDetailItem =
  | { readonly kind: "thought"; readonly item: ThoughtConversationItem }
  | { readonly kind: "action"; readonly action: ActionView };

/** Expandable evidence behind a collapsed supervisor action. */
export type ActionDetail =
  | { readonly kind: "raw"; readonly lines: readonly string[] }
  | { readonly kind: "actions"; readonly actions: readonly ActionView[] }
  | { readonly kind: "activity"; readonly items: readonly ActivityDetailItem[] }
  | {
      readonly kind: "terminalOutput";
      readonly output: TerminalOutputViewModel;
    }
  | {
      readonly kind: "diff";
      readonly diff: SessionDiff;
      readonly path?: string;
    }
  | { readonly kind: "files"; readonly paths: readonly string[] };

export type ActionStatus = "success" | "failure" | "running";

export interface ActionView {
  readonly id: string;
  readonly type: "cli" | "read" | "edit";
  readonly title: string;
  readonly meta?: string;
  readonly cli?: CliKind;
  readonly cliJobId?: string;
  readonly variant?: "dispatch";
  readonly level: "info" | "warn" | "error";
  readonly status: ActionStatus;
  readonly compressible?: boolean;
  readonly detail?: ActionDetail;
}

export interface TodoItemView {
  readonly id: string;
  readonly content: string;
  readonly status: "pending" | "in_progress" | "completed";
}

export interface ConversationVersionNavigation {
  readonly current: number;
  readonly total: number;
  readonly previousRunId?: string;
  readonly nextRunId?: string;
}

export interface ThoughtConversationItem {
  readonly kind: "thought";
  readonly id: string;
  readonly title: string;
  readonly text: string;
  readonly at: string;
  readonly status: "streaming" | "completed";
  readonly startedAt?: string;
  readonly completedAt?: string;
}

/** An ordered item in the supervisor conversation. */
export type ConversationItem =
  | {
      readonly kind: "user";
      readonly id: string;
      readonly runId: string;
      readonly text: string;
      readonly at: string;
      readonly attachments?: readonly SupervisorRunAttachmentRecord[];
      readonly versionNavigation?: ConversationVersionNavigation;
    }
  | {
      readonly kind: "supervisor";
      readonly id: string;
      readonly text: string;
      readonly at: string;
      readonly tone?: "default" | "muted" | "error";
    }
  | ThoughtConversationItem
  | {
      readonly kind: "action";
      readonly id: string;
      readonly at: string;
      readonly action: ActionView;
    };

export interface CliJobView {
  readonly id: string;
  readonly runId: string;
  readonly sessionId: string;
  readonly cli: CliKind;
  readonly description: string;
  readonly status: SupervisorCliJobStatus;
  readonly startedAt: string;
  readonly completedAt?: string;
  readonly stdout?: string;
  readonly stderr?: string;
  readonly stdoutTruncated?: boolean;
  readonly stderrTruncated?: boolean;
  readonly exitCode?: number;
}

export interface GroupDiscussionParticipantView {
  readonly participantId: string;
  readonly modelId: string;
  readonly displayName: string;
  readonly status: GroupDiscussionParticipantStatus;
  readonly stance?: string;
  readonly keyPoints: readonly string[];
  readonly risks: readonly string[];
  readonly recommendations: readonly string[];
  readonly rawOutput?: string;
  readonly error?: string;
}

export interface GroupDiscussionView {
  readonly id: string;
  readonly runId: string;
  readonly topic: string;
  readonly depth: GroupDiscussionDepth;
  readonly status: GroupDiscussionStatus;
  readonly participants: readonly GroupDiscussionParticipantView[];
  readonly summary?: string;
  readonly consensus: readonly string[];
  readonly disagreements: readonly string[];
  readonly risks: readonly string[];
  readonly recommendations: readonly string[];
  readonly startedAt: string;
  readonly completedAt?: string;
}
