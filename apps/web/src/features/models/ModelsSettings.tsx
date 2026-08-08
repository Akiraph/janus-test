import { useQueryClient } from "@tanstack/solid-query";
import Activity from "lucide-solid/icons/activity";
import ChevronDown from "lucide-solid/icons/chevron-down";
import Pencil from "lucide-solid/icons/pencil";
import Plus from "lucide-solid/icons/plus";
import Trash2 from "lucide-solid/icons/trash-2";
import { createSignal, For, Show } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { EmptyState } from "../../components/ui/EmptyState";
import { NotificationEvent, useNotifications } from "../../components/ui/notifications";
import { Select, type SelectOption } from "../../components/ui/Select";
import type { EmbeddedModelInput, ProviderInput, ProviderView } from "../../lib/api";
import {
  createProvider,
  deleteProvider,
  getErrorMessage,
  probeProvider,
  updateProvider,
} from "../../lib/api";
import { useProviders } from "../../lib/queries";
import "./models.css";

type ProviderClient = NonNullable<ProviderInput["client"]>;
type ProviderKind = ProviderInput["kind"];

type ProviderSection = {
  client: ProviderClient;
  title: string;
  description: string;
};

const PROVIDER_SECTIONS: readonly ProviderSection[] = [
  {
    client: "supervisor",
    title: "Supervisor",
    description: "Models used directly by the Supervisor.",
  },
  {
    client: "claude-code",
    title: "Claude Code",
    description: "Claude Code providers used by delegated jobs.",
  },
  {
    client: "codex",
    title: "Codex",
    description: "Codex providers used by delegated jobs.",
  },
];

const KIND_OPTIONS: readonly SelectOption[] = [
  { value: "anthropic", label: "Anthropic Messages" },
  { value: "openai_chat", label: "OpenAI Chat Completions" },
  { value: "openai_responses", label: "OpenAI Responses API" },
];

const KIND_BASE_URL_PLACEHOLDER: Record<ProviderKind, string> = {
  anthropic: "https://api.anthropic.com",
  openai_chat: "https://api.openai.com/v1",
  openai_responses: "https://api.openai.com/v1",
};

type ModelRow = EmbeddedModelInput;

function emptyModel(): ModelRow {
  return {
    display_name: "",
    upstream_model_id: "",
    supports_1m: false,
    supports_images: false,
    enabled: true,
  };
}

function clientLabel(client: ProviderClient): string {
  if (client === "claude-code") return "Claude Code";
  if (client === "codex") return "Codex";
  return "Supervisor";
}

export function ModelsSettings() {
  const providers = useProviders();
  const queryClient = useQueryClient();
  const notify = useNotifications().notify;
  const [openSections, setOpenSections] = createStore<Record<ProviderClient, boolean>>({
    supervisor: true,
    "claude-code": true,
    codex: true,
  });
  const [editing, setEditing] = createSignal<ProviderView | null>(null);
  const [formClient, setFormClient] = createSignal<ProviderClient>("supervisor");
  const [formOpen, setFormOpen] = createSignal(false);

  async function refresh() {
    await queryClient.invalidateQueries({ queryKey: ["model-providers"] });
  }

  async function removeProvider(id: string) {
    if (!confirm("Delete this provider and its models?")) return;
    try {
      await deleteProvider(id);
      notify("Provider deleted", { variant: "success" });
      await refresh();
    } catch (error) {
      notify(getErrorMessage(error, "Delete failed"), { variant: "danger" });
    }
  }

  async function probe(id: string) {
    try {
      const result = await probeProvider(id);
      notify(`${result.status}: ${result.detail}`);
    } catch (error) {
      notify(getErrorMessage(error, "Probe failed"), { variant: "danger" });
    }
  }

  function openCreate(client: ProviderClient) {
    setEditing(null);
    setFormClient(client);
    setFormOpen(true);
  }

  function openEdit(provider: ProviderView) {
    setEditing(provider);
    setFormClient(provider.client);
    setFormOpen(true);
  }

  function providersFor(client: ProviderClient): ProviderView[] {
    return (providers.data ?? []).filter((provider) => provider.client === client);
  }

  return (
    <div class="panel model-provider-settings">
      <div class="provider-section-stack">
        <For each={PROVIDER_SECTIONS}>
          {(section) => (
            <section class="settings-group provider-client-section">
              <div class="provider-client-section-header">
                <button
                  class="settings-group-trigger"
                  type="button"
                  aria-expanded={openSections[section.client]}
                  aria-controls={`provider-section-body-${section.client}`}
                  onClick={() => setOpenSections(section.client, !openSections[section.client])}
                >
                  <ChevronDown classList={{ collapsed: !openSections[section.client] }} size={16} />
                  <div>
                    <span class="settings-group-title">{section.title}</span>
                    <small>{section.description}</small>
                  </div>
                </button>
                <Button
                  variant="outline"
                  class="provider-client-section-add"
                  onClick={() => openCreate(section.client)}
                >
                  <Plus size={16} />
                  Add {section.title} provider
                </Button>
              </div>

              <Show when={openSections[section.client]}>
                <div class="settings-group-body" id={`provider-section-body-${section.client}`}>
                  <Show
                    when={!providers.isPending}
                    fallback={
                      <p class="surface-note" role="status" aria-label="Loading...">
                        Loading...
                      </p>
                    }
                  >
                    <Show
                      when={providersFor(section.client).length > 0}
                      fallback={
                        <EmptyState
                          icon={Plus}
                          title={`No ${section.title} providers`}
                          description={`Add a ${section.title} provider for this client.`}
                        />
                      }
                    >
                      <div class="record-list">
                        <For each={providersFor(section.client)}>
                          {(provider) => (
                            <article class="record-card provider-card">
                              <div class="provider-card-main">
                                <div class="record-copy">
                                  <div class="record-title">
                                    <h3>{provider.display_name}</h3>
                                    <span class="record-chip">{clientLabel(provider.client)}</span>
                                  </div>
                                  <p>{provider.base_url}</p>
                                </div>
                                <div class="record-actions">
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={() => void probe(provider.id)}
                                  >
                                    <Activity size={14} />
                                    Test
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    iconOnly
                                    aria-label={`Edit ${provider.display_name}`}
                                    onClick={() => openEdit(provider)}
                                  >
                                    <Pencil size={16} />
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    iconOnly
                                    aria-label={`Delete ${provider.display_name}`}
                                    onClick={() => void removeProvider(provider.id)}
                                  >
                                    <Trash2 size={16} />
                                  </Button>
                                </div>
                              </div>

                              <div class="provider-models">
                                <div class="provider-models-heading">
                                  <span>Models</span>
                                </div>
                                <Show
                                  when={provider.models.length > 0}
                                  fallback={
                                    <p class="provider-model-empty">No models configured.</p>
                                  }
                                >
                                  <div class="provider-model-list">
                                    <For each={provider.models}>
                                      {(model) => (
                                        <div class="provider-model-row">
                                          <div class="model-map-chip">
                                            <strong>{model.display_name}</strong>
                                            <span aria-hidden>→</span>
                                            <span>{model.upstream_model_id}</span>
                                          </div>
                                          <div class="record-chips">
                                            <Show when={model.supports_1m}>
                                              <span>1M</span>
                                            </Show>
                                            <Show when={model.supports_images}>
                                              <span>images</span>
                                            </Show>
                                          </div>
                                        </div>
                                      )}
                                    </For>
                                  </div>
                                </Show>
                              </div>
                            </article>
                          )}
                        </For>
                      </div>
                    </Show>
                  </Show>
                </div>
              </Show>
            </section>
          )}
        </For>
      </div>

      <Show when={formOpen()}>
        <ProviderForm
          provider={editing()}
          client={formClient()}
          close={() => setFormOpen(false)}
          saved={async () => {
            setFormOpen(false);
            await refresh();
          }}
        />
      </Show>
    </div>
  );
}

interface ProviderFormProps {
  provider: ProviderView | null;
  client: ProviderClient;
  close: () => void;
  saved: () => Promise<void>;
}

function ProviderForm(props: ProviderFormProps) {
  const notify = useNotifications().notify;
  const isEditing = () => props.provider !== null;
  const hasKey = () => props.provider?.api_key_is_set ?? false;

  const [name, setName] = createSignal("");
  const [client, setClient] = createSignal<ProviderClient>(props.provider?.client ?? props.client);
  const [kind, setKind] = createSignal<ProviderKind>("anthropic");
  const [url, setUrl] = createSignal("");
  const [key, setKey] = createSignal("");
  const [editingKey, setEditingKey] = createSignal(false);
  const [models, setModels] = createStore<ModelRow[]>([emptyModel()]);
  const [error, setError] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);

  // Seed the form whenever it opens for a given provider.
  const seed = (provider: ProviderView | null) => {
    if (provider) {
      setName(provider.display_name);
      setClient(provider.client);
      setKind(provider.kind);
      setUrl(provider.base_url);
      setEditingKey(false);
      setKey("");
      setModels(
        provider.models.length > 0
          ? provider.models.map((model) => ({
              display_name: model.display_name,
              upstream_model_id: model.upstream_model_id,
              supports_1m: model.supports_1m,
              supports_images: model.supports_images,
              enabled: model.enabled,
            }))
          : [emptyModel()],
      );
    } else {
      setName("");
      setClient(props.client);
      setKind("anthropic");
      setUrl("");
      setEditingKey(false);
      setKey("");
      setModels([emptyModel()]);
    }
    setError("");
  };
  // The dialog is mounted fresh on each open (parent gates it with <Show>),
  // so reseeding once at mount with the current provider target is correct.
  seed(props.provider);

  const updateModel = (index: number, patch: Partial<ModelRow>) => setModels(index, patch);
  const addModel = () => setModels(produce((rows) => rows.push(emptyModel())));
  const removeModel = (index: number) =>
    setModels(
      produce((rows) => {
        if (rows.length > 1) rows.splice(index, 1);
      }),
    );

  function buildInput(): ProviderInput {
    const cleanedModels: EmbeddedModelInput[] = models
      .map((row) => ({
        display_name: row.display_name.trim(),
        upstream_model_id: row.upstream_model_id.trim(),
        supports_1m: row.supports_1m ?? false,
        supports_images: row.supports_images ?? false,
        enabled: row.enabled ?? true,
      }))
      .filter((row) => row.display_name && row.upstream_model_id);

    const input: ProviderInput = {
      client: client(),
      kind: kind(),
      display_name: name().trim(),
      base_url: url().trim(),
      models: cleanedModels,
      enabled: true,
    };
    // Send a new key only when the user typed one; omit to keep the stored key.
    if (key().trim()) input.api_key = key().trim();
    return input;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!name().trim()) {
      setError("Name is required");
      return;
    }
    // A new provider requires a key; an existing one can keep its stored key.
    if (!isEditing() && !key().trim()) {
      setError("API key is required");
      return;
    }
    setSubmitting(true);
    try {
      const input = buildInput();
      if (isEditing() && props.provider) {
        await updateProvider(props.provider.id, input);
        notify("Provider updated", { variant: "success" });
      } else {
        await createProvider(input);
        notify("Provider added", { variant: "success" });
      }
      await props.saved();
    } catch (value) {
      setError(getErrorMessage(value, "Provider could not be saved"));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog
      title={
        isEditing()
          ? `Edit ${clientLabel(client())} provider`
          : `Add ${clientLabel(client())} provider`
      }
      description="Configure the upstream endpoint, API credential, and models."
      close={props.close}
    >
      <form class="dialog-form" onSubmit={submit}>
        <div class="dialog-form-grid">
          <div>
            <span class="field-label">Name</span>
            <input
              id="provider-name"
              class="ui-input"
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              aria-label="Name"
              required
            />
          </div>
          <div>
            <span class="field-label">API Format</span>
            <Select
              value={kind()}
              options={KIND_OPTIONS}
              onChange={(value) => setKind(value as ProviderKind)}
              aria-label="API Format"
            />
          </div>
          <div class="full-field">
            <span class="field-label">Base URL</span>
            <input
              id="provider-base-url"
              class="ui-input"
              type="url"
              value={url()}
              onInput={(event) => setUrl(event.currentTarget.value)}
              placeholder={KIND_BASE_URL_PLACEHOLDER[kind()]}
              aria-label="Base URL"
              required
            />
          </div>
          <div class="full-field">
            <span class="field-label">API key</span>
            <Show
              when={isEditing() && hasKey() && !editingKey()}
              fallback={
                <input
                  id="provider-api-key"
                  class="ui-input"
                  type="password"
                  value={key()}
                  onInput={(event) => setKey(event.currentTarget.value)}
                  placeholder={isEditing() ? "Enter a new API key" : "sk-..."}
                  autocomplete="off"
                  aria-label="API key"
                  required={!isEditing() || !hasKey()}
                />
              }
            >
              <span class="api-key-preview">
                <span class="api-key-preview-value">
                  {props.provider?.api_key_preview ?? "Stored API key"}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  iconOnly
                  aria-label="Change API key"
                  onClick={() => setEditingKey(true)}
                >
                  <Pencil size={14} />
                </Button>
              </span>
            </Show>
          </div>
        </div>

        <div class="provider-form-models">
          <div class="provider-form-models-heading">
            <span>Models</span>
            <Button variant="ghost" size="sm" type="button" onClick={addModel}>
              <Plus size={14} />
              Add model
            </Button>
          </div>
          <For each={models}>
            {(model, index) => (
              <div class="provider-form-model-row">
                <input
                  class="ui-input"
                  aria-label="Model name"
                  placeholder="Display name"
                  value={model.display_name}
                  onInput={(event) =>
                    updateModel(index(), { display_name: event.currentTarget.value })
                  }
                />
                <input
                  class="ui-input"
                  aria-label="Upstream model ID"
                  placeholder="Upstream model ID"
                  value={model.upstream_model_id}
                  onInput={(event) =>
                    updateModel(index(), { upstream_model_id: event.currentTarget.value })
                  }
                />
                <label class="model-chip-check" title="Supports 1m context">
                  <input
                    type="checkbox"
                    checked={model.supports_1m}
                    onChange={(event) =>
                      updateModel(index(), { supports_1m: event.currentTarget.checked })
                    }
                  />
                  1M
                </label>
                <label class="model-chip-check" title="Supports images">
                  <input
                    type="checkbox"
                    checked={model.supports_images}
                    onChange={(event) =>
                      updateModel(index(), { supports_images: event.currentTarget.checked })
                    }
                  />
                  Images
                </label>
                <Button
                  variant="ghost"
                  size="sm"
                  iconOnly
                  type="button"
                  aria-label="Remove model"
                  disabled={models.length === 1}
                  onClick={() => removeModel(index())}
                >
                  <Trash2 size={14} />
                </Button>
              </div>
            )}
          </For>
        </div>

        <NotificationEvent message={error()} variant="danger" />
        <div class="dialog-footer">
          <Button variant="outline" type="button" onClick={props.close}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={submitting()}>
            {isEditing() ? "Save changes" : "Add provider"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
