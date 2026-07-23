import type {
  ModelGatewayClient,
  ModelProviderRecord,
  TestModelProviderInput,
  UpsertModelProviderInput,
} from "@janus/shared";
import {
  modelConfigModelId,
  modelConfigReasoningEffort,
  modelReasoningEffortPresets,
} from "@janus/shared";
import * as DropdownMenuPrimitive from "@radix-ui/react-dropdown-menu";
import {
  Check,
  ChevronDown,
  FlaskConical,
  Pencil,
  Plus,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/ui/button";
import { Dialog } from "../../components/ui/dialog";
import { Input } from "../../components/ui/input";
import { Select } from "../../components/ui/select";
import { useTestProvider } from "./hooks/useTestProvider";
import { useUpsertProvider } from "./hooks/useUpsertProvider";
import { CLIENT_LABELS, deriveAuthMode } from "./provider-clients";

interface AliasRow {
  readonly id: string;
  readonly alias: string;
  readonly model: string;
  readonly reasoningEffort: string;
}

export interface ProviderFormProps {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly provider: ModelProviderRecord | null;
  /** Which client this provider is being configured for (used when adding). */
  readonly client: ModelGatewayClient;
}

/**
 * ProviderForm — Dialog for adding or editing a model provider.
 *
 * The form adapts to the target client:
 * - `claude-code` exposes the full opus/sonnet/haiku model mapping.
 * - `codex` exposes the OpenAI wire-API format and a single default model.
 * - `supervisor` lets you add multiple models, each with a free-form alias.
 *
 * Auth mode is derived automatically from the wire API (Anthropic → x-api-key,
 * OpenAI → bearer), so it is no longer a user-facing field.
 */
export function ProviderForm({
  open,
  onOpenChange,
  provider,
  client,
}: ProviderFormProps) {
  const isEditing = provider !== null;
  const effectiveClient = provider?.client ?? client;
  const supportsModelMapping = effectiveClient === "claude-code";
  const supportsWireApi =
    effectiveClient === "codex" || effectiveClient === "supervisor";
  const supportsAliasList = effectiveClient === "supervisor";

  const upsertMutation = useUpsertProvider();
  const testMutation = useTestProvider();

  // Stable ids for supervisor alias rows so React keys survive add/remove.
  const formRef = useRef<HTMLFormElement>(null);
  const rowIdRef = useRef(0);
  const newRow = (
    alias = "",
    model = "",
    reasoningEffort = "none",
  ): AliasRow => ({
    id: `row-${rowIdRef.current++}`,
    alias,
    model,
    reasoningEffort,
  });

  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [wireApi, setWireApi] = useState<string>("anthropic");
  const [apiKey, setApiKey] = useState("");
  const [editingApiKey, setEditingApiKey] = useState(false);
  const [defaultModel, setDefaultModel] = useState("");
  const [opusModel, setOpusModel] = useState("");
  const [sonnetModel, setSonnetModel] = useState("");
  const [haikuModel, setHaikuModel] = useState("");
  const [discussionEnabled, setDiscussionEnabled] = useState(false);
  // Supervisor only: arbitrary {alias → model} rows.
  const [aliasRows, setAliasRows] = useState<AliasRow[]>(() => [
    newRow("default"),
  ]);
  const [testResult, setTestResult] = useState<{
    readonly tone: "success" | "error";
    readonly message: string;
  } | null>(null);

  // Sync form state when the provider (or target client) changes.
  useEffect(() => {
    if (!open) {
      return;
    }

    if (provider) {
      setName(provider.name);
      setBaseUrl(provider.upstreamBaseUrl);
      setWireApi(
        effectiveClient === "codex"
          ? "responses"
          : (provider.wireApi ?? "anthropic"),
      );
      setApiKey("");
      setEditingApiKey(false);
      setDefaultModel(
        provider.models.default === undefined
          ? ""
          : modelConfigModelId(provider.models.default),
      );
      setOpusModel(
        provider.models.opus === undefined
          ? ""
          : modelConfigModelId(provider.models.opus),
      );
      setSonnetModel(
        provider.models.sonnet === undefined
          ? ""
          : modelConfigModelId(provider.models.sonnet),
      );
      setHaikuModel(
        provider.models.haiku === undefined
          ? ""
          : modelConfigModelId(provider.models.haiku),
      );
      setDiscussionEnabled(provider.discussionEnabled);
      setAliasRows(
        Object.entries(provider.models).map(([alias, config]) => ({
          id: `row-${rowIdRef.current++}`,
          alias,
          model: modelConfigModelId(config),
          reasoningEffort: modelConfigReasoningEffort(config) ?? "none",
        })),
      );
    } else {
      setName("");
      setBaseUrl("");
      setWireApi(client === "codex" ? "responses" : "anthropic");
      setApiKey("");
      setEditingApiKey(true);
      setDefaultModel("");
      setOpusModel("");
      setSonnetModel("");
      setHaikuModel("");
      setDiscussionEnabled(false);
      setAliasRows([
        {
          id: `row-${rowIdRef.current++}`,
          alias: "default",
          model: "",
          reasoningEffort: "none",
        },
      ]);
    }
    setTestResult(null);
  }, [open, provider, client, effectiveClient]);

  const updateRow = (index: number, patch: Partial<AliasRow>) => {
    setAliasRows((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    );
  };
  const addRow = () => setAliasRows((rows) => [...rows, newRow()]);
  const removeRow = (index: number) =>
    setAliasRows((rows) => rows.filter((_, i) => i !== index));

  const buildProviderRequest = (): UpsertModelProviderInput => {
    let models: UpsertModelProviderInput["models"];
    if (supportsAliasList) {
      const valid = aliasRows
        .map((row) => ({
          alias: row.alias.trim(),
          model: row.model.trim(),
          reasoningEffort: row.reasoningEffort.trim() || "none",
        }))
        .filter((row) => row.alias.length > 0 && row.model.length > 0);

      if (valid.length === 0) {
        throw new Error("Add at least one model before continuing.");
      }

      const aliases = new Set<string>();
      models = {};
      for (const row of valid) {
        if (aliases.has(row.alias)) {
          throw new Error("Model aliases must be unique.");
        }
        aliases.add(row.alias);
        models[row.alias] = {
          model: row.model,
          reasoningEffort: row.reasoningEffort,
        };
      }
    } else {
      models = { default: defaultModel.trim() };
      if (supportsModelMapping) {
        if (opusModel.trim()) models.opus = opusModel.trim();
        if (sonnetModel.trim()) models.sonnet = sonnetModel.trim();
        if (haikuModel.trim()) models.haiku = haikuModel.trim();
      }
    }

    const request: UpsertModelProviderInput = {
      ...(isEditing ? { id: provider.id } : {}),
      client: effectiveClient,
      name: name.trim(),
      upstreamBaseUrl: baseUrl.trim(),
      ...(apiKey.trim() ? { apiKey: apiKey.trim() } : {}),
      authMode: deriveAuthMode(supportsWireApi ? wireApi : "anthropic"),
      models,
      enabled: true,
      discussionEnabled:
        effectiveClient === "supervisor" ? discussionEnabled : false,
      priority: 0,
    };

    // wireApi marks OpenAI-compatible upstreams; undefined means Anthropic.
    if (supportsWireApi && wireApi !== "anthropic") {
      request.wireApi = wireApi as "chat" | "responses";
    }

    return request;
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    let request: UpsertModelProviderInput;
    try {
      request = buildProviderRequest();
    } catch (error) {
      setTestResult({ tone: "error", message: errorMessage(error) });
      return;
    }

    try {
      await upsertMutation.mutateAsync(request);
      onOpenChange(false);
    } catch {
      // Error is surfaced below via upsertMutation.isError.
    }
  };

  const handleTest = async () => {
    if (formRef.current !== null && !formRef.current.reportValidity()) {
      return;
    }

    let request: TestModelProviderInput;
    try {
      const providerRequest = buildProviderRequest();
      request = {
        ...providerRequest,
        modelAlias:
          providerRequest.models.default === undefined
            ? Object.keys(providerRequest.models)[0]
            : "default",
      };
    } catch (error) {
      setTestResult({ tone: "error", message: errorMessage(error) });
      return;
    }

    try {
      await testMutation.mutateAsync(request);
      setTestResult({
        tone: "success",
        message: "Test call succeeded.",
      });
    } catch (error) {
      setTestResult({ tone: "error", message: errorMessage(error) });
    }
  };

  const title = `${isEditing ? "Edit" : "Add"} ${CLIENT_LABELS[effectiveClient]} provider`;

  return (
    <Dialog open={open} onOpenChange={onOpenChange} title={title}>
      <form ref={formRef} onSubmit={handleSubmit} className="space-y-4">
        <Field id="provider-name" label="Name">
          <Input
            id="provider-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g., Claude Production"
            required
          />
        </Field>

        {supportsWireApi && (
          <Field id="provider-api-format" label="API Format">
            <Select
              options={
                effectiveClient === "supervisor"
                  ? [
                      { value: "anthropic", label: "Anthropic Messages" },
                      { value: "chat", label: "OpenAI Chat Completions" },
                      { value: "responses", label: "OpenAI Responses" },
                    ]
                  : [{ value: "responses", label: "OpenAI Responses" }]
              }
              value={wireApi}
              onValueChange={setWireApi}
            />
          </Field>
        )}

        <Field id="provider-base-url" label="Base URL">
          <Input
            id="provider-base-url"
            type="url"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={
              wireApi === "chat" || wireApi === "responses"
                ? "https://api.openai.com/v1"
                : "https://api.anthropic.com"
            }
            required
          />
        </Field>

        <Field
          id="provider-api-key"
          label="API Key"
          {...(isEditing
            ? {}
            : {
                hint: "Stored encrypted; it is never shown again after saving.",
              })}
        >
          {isEditing && provider.hasApiKey && !editingApiKey ? (
            <div className="flex min-h-9 items-center gap-1">
              <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                {provider.apiKeyPreview ?? "Stored API key"}
              </span>
              <button
                id="provider-api-key"
                type="button"
                aria-label="Change API key"
                onClick={() => setEditingApiKey(true)}
                className="flex h-8 w-8 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors duration-150 hover:text-foreground focus-visible:bg-muted focus-visible:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-border-accent/60 focus-visible:ring-offset-1 focus-visible:ring-offset-background"
              >
                <Pencil className="h-4 w-4" />
              </button>
            </div>
          ) : (
            <Input
              id="provider-api-key"
              type="password"
              autoComplete="off"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={isEditing ? "Enter a new API key" : "sk-..."}
              required={!isEditing || !provider?.hasApiKey}
            />
          )}
        </Field>

        {!supportsAliasList && (
          <Field
            id="provider-default-model"
            label={supportsModelMapping ? "Default model" : "Model"}
            hint="The model identifier to use by default."
          >
            <Input
              id="provider-default-model"
              value={defaultModel}
              onChange={(e) => setDefaultModel(e.target.value)}
              placeholder={
                wireApi === "chat" || wireApi === "responses"
                  ? "e.g., gpt-4o"
                  : "e.g., claude-3-5-sonnet-20241022"
              }
              required
            />
          </Field>
        )}

        {effectiveClient === "supervisor" && (
          <label className="flex items-start gap-2 rounded-md border border-border bg-muted/30 p-3">
            <input
              type="checkbox"
              checked={discussionEnabled}
              onChange={(event) => setDiscussionEnabled(event.target.checked)}
              className="mt-0.5 h-4 w-4 rounded-xs border-border accent-info"
            />
            <span className="min-w-0">
              <span className="block text-sm font-medium text-foreground">
                Available for group discussion
              </span>
              <span className="mt-0.5 block text-xs leading-relaxed text-muted-foreground">
                Supervisor may select this provider's models for advisory
                multi-model discussion.
              </span>
            </span>
          </label>
        )}

        {supportsAliasList && (
          <div className="space-y-3 rounded-lg border border-border bg-muted/30 p-3">
            <p className="text-xs font-medium text-foreground">
              Models
              <span className="ml-1 font-normal text-muted-foreground">
                — add models, aliases, and reasoning effort
              </span>
            </p>
            <div className="space-y-2">
              {aliasRows.map((row, index) => (
                <div key={row.id} className="flex items-center gap-2">
                  <Input
                    aria-label="Alias"
                    value={row.alias}
                    onChange={(e) =>
                      updateRow(index, { alias: e.target.value })
                    }
                    placeholder="alias"
                    className="w-32 shrink-0"
                  />
                  <span className="text-muted-foreground" aria-hidden>
                    →
                  </span>
                  <Input
                    aria-label="Model"
                    value={row.model}
                    onChange={(e) =>
                      updateRow(index, { model: e.target.value })
                    }
                    placeholder={
                      wireApi === "chat" || wireApi === "responses"
                        ? "e.g., gpt-4o"
                        : "e.g., claude-opus-4-8"
                    }
                    className="flex-1"
                  />
                  <ReasoningEffortInput
                    value={row.reasoningEffort}
                    onValueChange={(reasoningEffort) =>
                      updateRow(index, { reasoningEffort })
                    }
                  />
                  <button
                    type="button"
                    aria-label="Remove model"
                    onClick={() => removeRow(index)}
                    disabled={aliasRows.length === 1}
                    className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    <X className="h-4 w-4" />
                  </button>
                </div>
              ))}
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={addRow}
              className="gap-1.5"
            >
              <Plus className="h-3.5 w-3.5" />
              Add model
            </Button>
          </div>
        )}

        {supportsModelMapping && (
          <div className="space-y-3 rounded-lg border border-border bg-muted/30 p-3">
            <p className="text-xs font-medium text-foreground">
              Model mapping
              <span className="ml-1 font-normal text-muted-foreground">
                — map Claude Code aliases to upstream models (optional)
              </span>
            </p>
            <Field id="provider-opus" label="Opus" compact>
              <Input
                id="provider-opus"
                value={opusModel}
                onChange={(e) => setOpusModel(e.target.value)}
                placeholder="e.g., claude-opus-4-20250514"
              />
            </Field>
            <Field id="provider-sonnet" label="Sonnet" compact>
              <Input
                id="provider-sonnet"
                value={sonnetModel}
                onChange={(e) => setSonnetModel(e.target.value)}
                placeholder="e.g., claude-sonnet-4-20250514"
              />
            </Field>
            <Field id="provider-haiku" label="Haiku" compact>
              <Input
                id="provider-haiku"
                value={haikuModel}
                onChange={(e) => setHaikuModel(e.target.value)}
                placeholder="e.g., claude-haiku-4-20250514"
              />
            </Field>
          </div>
        )}

        {upsertMutation.isError && (
          <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            Failed to save provider. Please try again.
          </div>
        )}

        {testResult && (
          <div
            className={
              testResult.tone === "success"
                ? "rounded-md bg-success-soft p-3 text-sm text-success"
                : "rounded-md bg-destructive/10 p-3 text-sm text-destructive"
            }
          >
            {testResult.message}
          </div>
        )}

        <div className="flex flex-wrap items-center justify-between gap-2 pt-4">
          <Button
            type="button"
            variant="outline"
            onClick={handleTest}
            disabled={testMutation.isPending || upsertMutation.isPending}
          >
            <FlaskConical className="h-4 w-4" />
            {testMutation.isPending ? "Testing..." : "Test call"}
          </Button>
          <div className="flex justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={upsertMutation.isPending}>
              {upsertMutation.isPending
                ? "Saving..."
                : isEditing
                  ? "Save changes"
                  : "Add provider"}
            </Button>
          </div>
        </div>
      </form>
    </Dialog>
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Request failed.";
}

interface ReasoningEffortInputProps {
  readonly value: string;
  readonly onValueChange: (value: string) => void;
}

function ReasoningEffortInput({
  value,
  onValueChange,
}: ReasoningEffortInputProps) {
  return (
    <DropdownMenuPrimitive.Root>
      <div className="relative w-36 shrink-0">
        <input
          aria-label="Reasoning effort"
          value={value}
          onChange={(event) => onValueChange(event.target.value)}
          placeholder="none"
          className="flex h-9 w-full rounded-sm border border-border bg-card px-3 py-2 pr-9 font-mono text-sm transition-[border-color,box-shadow] duration-200 ease-out placeholder:text-muted-foreground hover:border-border-strong focus-visible:border-border-strong focus-visible:outline-none focus-visible:shadow-focus"
        />
        <DropdownMenuPrimitive.Trigger asChild>
          <button
            type="button"
            aria-label="Select reasoning effort"
            className="absolute right-1 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-xs text-muted-foreground transition-colors duration-150 hover:text-foreground focus-visible:bg-muted focus-visible:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-border-accent/60 focus-visible:ring-offset-1 focus-visible:ring-offset-background"
          >
            <ChevronDown className="h-4 w-4" />
          </button>
        </DropdownMenuPrimitive.Trigger>
      </div>

      <DropdownMenuPrimitive.Portal>
        <DropdownMenuPrimitive.Content
          align="end"
          sideOffset={4}
          className="z-50 min-w-[9rem] overflow-hidden rounded-md border border-border bg-card p-1 text-foreground shadow-md data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
        >
          {modelReasoningEffortPresets.map((effort) => (
            <DropdownMenuPrimitive.Item
              key={effort}
              onSelect={() => onValueChange(effort)}
              className="relative flex cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1.5 font-mono text-sm outline-none transition-colors hover:bg-muted focus:bg-muted"
            >
              <span className="flex h-4 w-4 items-center justify-center">
                {value === effort ? <Check className="h-4 w-4" /> : null}
              </span>
              <span>{effort}</span>
            </DropdownMenuPrimitive.Item>
          ))}
        </DropdownMenuPrimitive.Content>
      </DropdownMenuPrimitive.Portal>
    </DropdownMenuPrimitive.Root>
  );
}

interface FieldProps {
  readonly id: string;
  readonly label: string;
  readonly hint?: string;
  readonly compact?: boolean;
  readonly children: React.ReactNode;
}

function Field({ id, label, hint, compact, children }: FieldProps) {
  return (
    <div className={compact ? "space-y-1" : "space-y-2"}>
      <label htmlFor={id} className="text-sm font-medium text-foreground">
        {label}
      </label>
      {children}
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}
