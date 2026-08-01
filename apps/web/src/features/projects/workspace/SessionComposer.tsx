import Loader2 from "lucide-solid/icons/loader-2";
import Paperclip from "lucide-solid/icons/paperclip";
import Send from "lucide-solid/icons/send";
import Square from "lucide-solid/icons/square";
import X from "lucide-solid/icons/x";
import { createEffect, createMemo, createSignal, For, Show, untrack } from "solid-js";
import { Button } from "../../../components/ui/Button";
import { useNotifications } from "../../../components/ui/notifications";
import { Select, type SelectOption } from "../../../components/ui/Select";
import type {
  AttachmentView,
  ContextUsageView,
  ProviderView,
  PublicLimits,
  ReasoningEffort,
  SessionModelPreference,
} from "../../../lib/api";
import { getErrorMessage } from "../../../lib/api";

export interface SessionMessageReceipt {
  route: string;
  turnId: string;
}

interface SessionComposerProps {
  delivery: "send" | "queue";
  disabled?: boolean;
  /** True when a Turn is actively running (queued or executing). When true
   * the primary action becomes Cancel (stop) instead of Send. */
  isRunning?: boolean;
  contextUsage: ContextUsageView | null;
  limits: PublicLimits | undefined;
  modelPreference: SessionModelPreference | null;
  providers: readonly ProviderView[];
  sessionId: string;
  onSubmit: (
    content: string,
    modelPreference: SessionModelPreference | null,
    attachmentIds: readonly string[],
  ) => Promise<SessionMessageReceipt>;
  onUploadAttachment: (sessionId: string, file: File) => Promise<AttachmentView>;
  onDeleteAttachment: (sessionId: string, attachmentId: string) => Promise<void>;
  onCancel?: (() => Promise<void>) | undefined;
}

const REASONING_OPTIONS: readonly SelectOption[] = [
  { value: "none", label: "No reasoning" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra high" },
  { value: "max", label: "Maximum" },
];

export function SessionComposer(props: SessionComposerProps) {
  const { notify } = useNotifications();
  const [draft, setDraft] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);
  const [receipt, setReceipt] = createSignal<SessionMessageReceipt | null>(null);
  const [modelValue, setModelValue] = createSignal("");
  const [reasoningEffort, setReasoningEffort] = createSignal<ReasoningEffort>("none");
  const [attachments, setAttachments] = createSignal<AttachmentView[]>([]);
  const [uploading, setUploading] = createSignal(false);
  const [canceling, setCanceling] = createSignal(false);
  let textarea: HTMLTextAreaElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let attachmentSessionId = props.sessionId;

  const availableModels = createMemo(() =>
    props.providers
      .filter((provider) => provider.enabled)
      .flatMap((provider) =>
        provider.models
          .filter((model) => model.enabled)
          .map((model) => ({
            value: modelKey(provider.id, model.upstream_model_id),
            label: `${provider.display_name}/${model.display_name}`,
            providerId: provider.id,
            providerKind: provider.kind,
            upstreamModelId: model.upstream_model_id,
          })),
      ),
  );
  const modelOptions = createMemo<readonly SelectOption[]>(() => availableModels());
  const selectedModel = createMemo(() =>
    availableModels().find((model) => model.value === modelValue()),
  );
  const supportsReasoning = () => true; // Let backend decide; API will error if unsupported

  createEffect(() => {
    const models = availableModels();
    const preference = props.modelPreference;
    const preferred = preference
      ? modelKey(preference.provider_id, preference.upstream_model_id)
      : null;
    const current = untrack(modelValue);
    if (preferred && models.some((model) => model.value === preferred)) {
      setModelValue(preferred);
      setReasoningEffort(preference?.reasoning_effort ?? "none");
    } else if (!models.some((model) => model.value === current)) {
      setModelValue(models[0]?.value ?? "");
      setReasoningEffort("none");
    }
  });

  createEffect(() => {
    const sessionId = props.sessionId;
    if (sessionId === attachmentSessionId) return;
    const stale = attachments();
    setAttachments([]);
    for (const attachment of stale) {
      void props.onDeleteAttachment(attachmentSessionId, attachment.id);
    }
    attachmentSessionId = sessionId;
  });

  const hasContent = () => Boolean(draft().trim()) || attachments().length > 0;
  const canSubmit = () => !props.disabled && !submitting() && !uploading() && hasContent();
  const actionLabel = () => (props.delivery === "queue" ? "Queue message" : "Send message");

  function resize() {
    if (!textarea) return;
    textarea.style.height = "auto";
    textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
  }

  async function submit() {
    const content = draft().trim();
    if (!canSubmit()) return;

    setSubmitting(true);
    setReceipt(null);
    try {
      const result = await props.onSubmit(
        content,
        selectedPreference(),
        attachments().map((attachment) => attachment.id),
      );
      setDraft("");
      setAttachments([]);
      setReceipt(result);
      if (textarea) textarea.style.height = "auto";
    } catch (cause) {
      notify(getErrorMessage(cause, "Message was not accepted"), {
        variant: "danger",
      });
    } finally {
      setSubmitting(false);
    }
  }

  async function addFiles(files: FileList | null) {
    if (!files || files.length === 0) return;
    const maxAttachments = props.limits?.max_attachments ?? 20;
    const maxFileBytes = props.limits?.max_file_bytes ?? 20 * 1024 * 1024;
    const selected = Array.from(files).slice(0, Math.max(0, maxAttachments - attachments().length));
    if (selected.length < files.length) {
      notify(`A message supports at most ${maxAttachments} attachments`, { variant: "warning" });
    }
    setUploading(true);
    setReceipt(null);
    try {
      for (const file of selected) {
        if (file.size > maxFileBytes) {
          notify(`${file.name} exceeds the ${formatBytes(maxFileBytes)} attachment limit`, {
            variant: "warning",
          });
          continue;
        }
        try {
          const attachment = await props.onUploadAttachment(props.sessionId, file);
          setAttachments((current) => [...current, attachment]);
        } catch (cause) {
          notify(getErrorMessage(cause, `${file.name} could not be uploaded`), {
            variant: "danger",
          });
        }
      }
    } finally {
      setUploading(false);
      if (fileInput) fileInput.value = "";
    }
  }

  async function cancel() {
    if (!props.onCancel || canceling()) return;
    setCanceling(true);
    setReceipt(null);
    try {
      await props.onCancel();
    } catch (cause) {
      notify(getErrorMessage(cause, "Turn cancellation was not accepted"), {
        variant: "danger",
      });
    } finally {
      setCanceling(false);
    }
  }

  async function removeAttachment(attachment: AttachmentView) {
    try {
      await props.onDeleteAttachment(props.sessionId, attachment.id);
      setAttachments((current) => current.filter((value) => value.id !== attachment.id));
    } catch (cause) {
      notify(getErrorMessage(cause, "Attachment could not be removed"), {
        variant: "danger",
      });
    }
  }

  function selectedPreference(): SessionModelPreference | null {
    const model = selectedModel();
    if (!model) return null;
    return {
      provider_id: model.providerId,
      upstream_model_id: model.upstreamModelId,
      reasoning_effort: supportsReasoning() ? reasoningEffort() : "none",
    };
  }

  const contextLabel = () => {
    const usage = props.contextUsage;
    if (!usage || usage.context_limit <= 0) return "--";
    return `${Math.min(999, Math.round((usage.estimated_input_tokens / usage.context_limit) * 100))}%`;
  };

  const contextTitle = () => {
    const usage = props.contextUsage;
    if (!usage) return "Context usage is not available yet";
    return `${usage.estimated_input_tokens.toLocaleString()} of ${usage.context_limit.toLocaleString()} context tokens used`;
  };

  return (
    <form
      class="session-composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <Show when={receipt()?.route === "queued"}>
        <p class="session-composer__status" role="status">
          Message queued
        </p>
      </Show>
      <Show when={attachments().length > 0}>
        <div class="session-composer__attachments">
          <For each={attachments()}>
            {(attachment) => (
              <span class="session-composer__attachment">
                <span title={attachment.name}>{attachment.name}</span>
                <small>{formatBytes(attachment.byte_size)}</small>
                <button
                  type="button"
                  aria-label={`Remove ${attachment.name}`}
                  disabled={submitting()}
                  onClick={() => void removeAttachment(attachment)}
                >
                  <X size={12} />
                </button>
              </span>
            )}
          </For>
        </div>
      </Show>
      <textarea
        ref={(element) => {
          textarea = element;
        }}
        class="session-composer__input"
        rows={1}
        placeholder={props.delivery === "queue" ? "Queue a message..." : "Send a message..."}
        value={draft()}
        disabled={submitting()}
        onInput={(event) => {
          setDraft(event.currentTarget.value);
          setReceipt(null);
          resize();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            void submit();
          }
        }}
      />
      <div class="session-composer__bar">
        <div class="session-composer__controls">
          <input
            ref={fileInput}
            class="session-composer__file-input"
            type="file"
            multiple
            onChange={(event) => void addFiles(event.currentTarget.files)}
          />
          <Button
            type="button"
            variant="ghost"
            size="sm"
            iconOnly
            disabled={
              submitting() ||
              uploading() ||
              Boolean(props.disabled) ||
              attachments().length >= (props.limits?.max_attachments ?? 20)
            }
            aria-label={uploading() ? "Uploading attachment" : "Add attachment"}
            onClick={() => fileInput?.click()}
          >
            <Show when={uploading()} fallback={<Paperclip size={15} />}>
              <Loader2 size={15} class="ui-spinner" />
            </Show>
          </Button>
          <Select
            class="session-composer__model"
            aria-label="Model"
            value={modelValue()}
            options={modelOptions()}
            disabled={submitting() || Boolean(props.disabled)}
            onChange={(value) => {
              setModelValue(value);
            }}
          />
          <Select
            class="session-composer__reasoning"
            aria-label="Reasoning effort"
            value={reasoningEffort()}
            options={REASONING_OPTIONS}
            disabled={
              submitting() || Boolean(props.disabled) || !selectedModel() || !supportsReasoning()
            }
            onChange={(value) => setReasoningEffort(value as ReasoningEffort)}
          />
          <span class="session-composer__context" title={contextTitle()}>
            {contextLabel()}
          </span>
          <span class="session-composer__delivery">
            {props.delivery === "queue" ? "Next turn" : "Current session"}
          </span>
        </div>
        <Show
          when={props.isRunning && !hasContent()}
          fallback={
            <Button
              type="submit"
              variant="primary"
              size="sm"
              iconOnly
              disabled={!canSubmit()}
              aria-label={submitting() ? `${actionLabel()} in progress` : actionLabel()}
            >
              <Show when={submitting()} fallback={<Send size={16} />}>
                <Loader2 size={16} class="ui-spinner" />
              </Show>
            </Button>
          }
        >
          <Button
            type="button"
            variant="outline"
            size="sm"
            iconOnly
            disabled={submitting() || canceling()}
            aria-label={canceling() ? "Canceling turn" : "Cancel turn"}
            onClick={() => void cancel()}
          >
            <Show when={canceling()} fallback={<Square size={16} />}>
              <Loader2 size={16} class="ui-spinner" />
            </Show>
          </Button>
        </Show>
      </div>
    </form>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function modelKey(providerId: string, upstreamModelId: string): string {
  return `${providerId}\u0000${upstreamModelId}`;
}
