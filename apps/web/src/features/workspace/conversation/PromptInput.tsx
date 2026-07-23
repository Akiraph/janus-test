import {
  type SupervisorRunAttachmentInput,
  type SupervisorRunDeliveryIntent,
  supervisorRunAttachmentMaxBytes,
  supervisorRunAttachmentMaxCount,
} from "@janus/shared";
import { Loader2, Plus, Send, Square, X } from "lucide-react";
import {
  type ChangeEvent,
  type FormEvent,
  type KeyboardEvent,
  useRef,
  useState,
} from "react";
import { ComposerModelPicker } from "./ComposerModelPicker";
import { formatBytes } from "./format-bytes";

export interface PromptSubmitOptions {
  readonly deliveryIntent: SupervisorRunDeliveryIntent;
  readonly attachments: readonly SupervisorRunAttachmentInput[];
}

interface PromptInputProps {
  onSubmit: (
    message: string,
    options: PromptSubmitOptions,
  ) => Promise<void> | void;
  onCancel?: () => void;
  disabled?: boolean;
  cancelling?: boolean;
  running?: boolean;
}

interface SelectedFile {
  readonly id: string;
  readonly file: File;
}

export function PromptInput({
  onSubmit,
  onCancel,
  disabled = false,
  cancelling = false,
  running = false,
}: PromptInputProps) {
  const [message, setMessage] = useState("");
  const [files, setFiles] = useState<SelectedFile[]>([]);
  const [encodingFiles, setEncodingFiles] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const fileValidationError = validateSelectedFiles(files);
  const canSubmit =
    message.trim().length > 0 &&
    !disabled &&
    !encodingFiles &&
    fileValidationError === undefined;
  const primaryAction =
    running && message.trim().length === 0 ? "stop" : "send";

  const handleSubmit = async (e?: FormEvent) => {
    e?.preventDefault();

    const trimmed = message.trim();
    if (
      !trimmed ||
      disabled ||
      encodingFiles ||
      fileValidationError !== undefined
    ) {
      return;
    }

    setEncodingFiles(true);
    try {
      const attachments = await Promise.all(
        files.map(({ file }) => fileToAttachmentInput(file)),
      );

      await onSubmit(trimmed, {
        deliveryIntent: "queue",
        attachments,
      });
      setMessage("");
      setFiles([]);

      if (textareaRef.current) {
        textareaRef.current.style.height = "auto";
      }
    } finally {
      setEncodingFiles(false);
    }
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Cmd/Ctrl + Enter to submit
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      void handleSubmit();
    }
  };

  const handleInput = () => {
    // Auto-grow textarea
    const textarea = textareaRef.current;
    if (!textarea) return;

    textarea.style.height = "auto";
    textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
  };

  const handleFilesPicked = (e: ChangeEvent<HTMLInputElement>) => {
    const picked = e.target.files ? Array.from(e.target.files) : [];
    if (picked.length > 0) {
      setFiles((prev) => [
        ...prev,
        ...picked.map((file) => ({
          id: crypto.randomUUID(),
          file,
        })),
      ]);
    }
    // Allow re-selecting the same file.
    e.target.value = "";
  };

  const removeFile = (id: string) => {
    setFiles((prev) => prev.filter((item) => item.id !== id));
  };

  return (
    <form
      onSubmit={handleSubmit}
      className="rounded-md border border-border bg-background transition-[border-color,box-shadow] duration-200 ease-out focus-within:border-border-strong focus-within:shadow-focus"
    >
      {files.length > 0 && (
        <div className="flex flex-wrap gap-1.5 px-3 pt-3">
          {files.map(({ id, file }) => (
            <span
              key={id}
              className="inline-flex max-w-[12rem] items-center gap-1.5 rounded-sm bg-muted px-2 py-1 text-xs text-foreground"
            >
              <span className="truncate">{file.name}</span>
              <button
                type="button"
                aria-label={`Remove ${file.name}`}
                onClick={() => removeFile(id)}
                className="shrink-0 text-muted-foreground hover:text-foreground"
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
        </div>
      )}
      {fileValidationError === undefined ? null : (
        <div className="px-3 pt-2 text-xs text-destructive">
          {fileValidationError}
        </div>
      )}

      <textarea
        ref={textareaRef}
        value={message}
        onChange={(e) => setMessage(e.target.value)}
        onKeyDown={handleKeyDown}
        onInput={handleInput}
        disabled={disabled}
        placeholder="Send a message..."
        rows={1}
        className="w-full resize-none bg-transparent px-3 pt-3 text-sm outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50"
        style={{ minHeight: "44px", maxHeight: "200px" }}
      />

      <div className="flex items-center justify-between gap-2 px-2 pb-2 pt-1">
        <div className="flex min-w-0 items-center gap-1">
          <input
            ref={fileInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={handleFilesPicked}
          />
          <button
            type="button"
            aria-label="Attach files"
            onClick={() => fileInputRef.current?.click()}
            className="flex h-8 w-8 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          >
            <Plus className="h-4 w-4" />
          </button>
        </div>

        <div className="flex items-center gap-1">
          <ComposerModelPicker />
          {primaryAction === "stop" ? (
            <button
              type="button"
              onClick={onCancel}
              disabled={cancelling}
              className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-sm bg-border-accent text-foreground transition-colors hover:bg-border-accent/85 disabled:cursor-not-allowed disabled:opacity-50"
              aria-label="Stop run"
            >
              {cancelling ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Square className="h-4 w-4" />
              )}
            </button>
          ) : (
            <button
              type="submit"
              disabled={!canSubmit}
              className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-sm bg-border-accent text-foreground transition-colors hover:bg-border-accent/85 disabled:cursor-not-allowed disabled:opacity-50"
              aria-label="Send message"
            >
              {encodingFiles ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Send className="h-4 w-4" />
              )}
            </button>
          )}
        </div>
      </div>
    </form>
  );
}

async function fileToAttachmentInput(
  file: File,
): Promise<SupervisorRunAttachmentInput> {
  const contentBase64 = arrayBufferToBase64(await file.arrayBuffer());

  return {
    name: file.name,
    ...(file.type.length === 0 ? {} : { mediaType: file.type }),
    sizeBytes: file.size,
    contentBase64,
  };
}

function validateSelectedFiles(
  files: readonly SelectedFile[],
): string | undefined {
  if (files.length > supervisorRunAttachmentMaxCount) {
    return `Attach up to ${supervisorRunAttachmentMaxCount} files.`;
  }

  const oversized = files.find(
    ({ file }) => file.size > supervisorRunAttachmentMaxBytes,
  );
  if (oversized !== undefined) {
    return `${oversized.file.name} exceeds ${formatBytes(
      supervisorRunAttachmentMaxBytes,
    )}.`;
  }

  return undefined;
}

function arrayBufferToBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  const chunkSize = 0x8000;
  let binary = "";

  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }

  return btoa(binary);
}
