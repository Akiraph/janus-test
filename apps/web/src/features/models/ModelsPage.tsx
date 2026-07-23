import { useQueryClient } from "@tanstack/solid-query";
import { Activity, KeyRound, Plus, Trash2 } from "lucide-solid";
import { createSignal, For, Show } from "solid-js";
import type { ProviderInput } from "../../lib/api";
import {
  createModel,
  createProvider,
  deleteModel,
  deleteProvider,
  probeProvider,
} from "../../lib/api";
import { useModels, useProviders } from "../../lib/queries";

export function ModelsPage() {
  const providers = useProviders();
  const models = useModels();
  const queryClient = useQueryClient();
  const [notice, setNotice] = createSignal("");
  const [providerForm, setProviderForm] = createSignal(false);
  const [modelForm, setModelForm] = createSignal(false);
  async function removeProvider(id: string) {
    if (!confirm("Delete this provider?")) return;
    try {
      await deleteProvider(id);
      setNotice("Provider deleted");
      await queryClient.invalidateQueries({ queryKey: ["model-providers"] });
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Delete failed");
    }
  }
  async function removeModel(id: string) {
    if (!confirm("Delete this model?")) return;
    try {
      await deleteModel(id);
      setNotice("Model deleted");
      await queryClient.invalidateQueries({ queryKey: ["models"] });
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Delete failed");
    }
  }
  async function probe(id: string) {
    try {
      const result = await probeProvider(id);
      setNotice(`${result.status}: ${result.detail}`);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Probe failed");
    }
  }
  return (
    <div class="settings-page route-enter">
      <div class="page-heading">
        <div>
          <p class="eyebrow">Configuration</p>
          <h1>Models</h1>
          <p class="page-subtitle">
            Manage provider credentials and the models available to Janus.
          </p>
        </div>
        <Show when={notice()}>
          <span class="notice" role="status">
            {notice()}
          </span>
        </Show>
      </div>
      <section class="settings-section">
        <div class="section-heading compact">
          <div>
            <p class="eyebrow">Connections</p>
            <h2>Providers</h2>
          </div>
          <button
            class="secondary-button"
            type="button"
            onClick={() => setProviderForm(!providerForm())}
          >
            <Plus size={16} />
            Add provider
          </button>
        </div>
        <Show when={providerForm()}>
          <ProviderForm
            done={async () => {
              setProviderForm(false);
              await queryClient.invalidateQueries({ queryKey: ["model-providers"] });
            }}
          />
        </Show>
        <Show when={providers.isPending}>
          <p class="muted-copy">Loading providers...</p>
        </Show>
        <Show when={providers.data?.length === 0}>
          <p class="muted-copy">No providers configured.</p>
        </Show>
        <div class="settings-list">
          <For each={providers.data}>
            {(provider) => (
              <div class="settings-row">
                <div class="row-icon">
                  <KeyRound size={16} />
                </div>
                <div class="row-copy">
                  <strong>{provider.display_name}</strong>
                  <span>
                    {provider.kind} · {provider.base_url}
                  </span>
                </div>
                <span class={`status-chip ${provider.api_key_is_set ? "success" : "muted"}`}>
                  {provider.api_key_is_set ? "Key set" : "No key"}
                </span>
                <button
                  class="icon-button"
                  type="button"
                  aria-label={`Probe ${provider.display_name}`}
                  title="Probe provider"
                  onClick={() => void probe(provider.id)}
                >
                  <Activity size={16} />
                </button>
                <button
                  class="icon-button danger-button"
                  type="button"
                  aria-label={`Delete ${provider.display_name}`}
                  title="Delete provider"
                  onClick={() => void removeProvider(provider.id)}
                >
                  <Trash2 size={16} />
                </button>
              </div>
            )}
          </For>
        </div>
      </section>
      <section class="settings-section">
        <div class="section-heading compact">
          <div>
            <p class="eyebrow">Routing</p>
            <h2>Models</h2>
          </div>
          <button
            class="secondary-button"
            type="button"
            disabled={!providers.data?.length}
            onClick={() => setModelForm(!modelForm())}
          >
            <Plus size={16} />
            Add model
          </button>
        </div>
        <Show when={modelForm()}>
          <ModelForm
            providers={providers.data ?? []}
            done={async () => {
              setModelForm(false);
              await queryClient.invalidateQueries({ queryKey: ["models"] });
            }}
          />
        </Show>
        <div class="settings-list">
          <For each={models.data}>
            {(model) => (
              <div class="settings-row">
                <div class="row-icon">
                  <Activity size={16} />
                </div>
                <div class="row-copy">
                  <strong>{model.display_name}</strong>
                  <span>
                    {model.upstream_model_id} · {model.context_window}
                  </span>
                </div>
                <span class={`status-chip ${model.enabled ? "success" : "muted"}`}>
                  {model.enabled ? "Enabled" : "Disabled"}
                </span>
                <button
                  class="icon-button danger-button"
                  type="button"
                  aria-label={`Delete ${model.display_name}`}
                  title="Delete model"
                  onClick={() => void removeModel(model.id)}
                >
                  <Trash2 size={16} />
                </button>
              </div>
            )}
          </For>
        </div>
      </section>
    </div>
  );
}

function ProviderForm(props: { done: () => Promise<void> }) {
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal<ProviderInput["kind"]>("openai_compatible");
  const [url, setUrl] = createSignal("https://api.openai.com/v1/");
  const [key, setKey] = createSignal("");
  const [supports, setSupports] = createSignal(false);
  const [error, setError] = createSignal("");
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    try {
      await createProvider({
        display_name: name(),
        kind: kind(),
        base_url: url(),
        api_key: key(),
        supports_1m: supports(),
        enabled: true,
      });
      await props.done();
    } catch (value) {
      setError(value instanceof Error ? value.message : "Provider could not be saved");
    }
  }
  return (
    <form class="inline-form" onSubmit={submit}>
      <label>
        Name
        <input value={name()} onInput={(e) => setName(e.currentTarget.value)} required />
      </label>
      <label>
        Type
        <select
          value={kind()}
          onChange={(e) => setKind(e.currentTarget.value as ProviderInput["kind"])}
        >
          <option value="openai_compatible">OpenAI compatible</option>
          <option value="anthropic">Anthropic</option>
        </select>
      </label>
      <label>
        Base URL
        <input type="url" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} required />
      </label>
      <label>
        API key
        <input
          type="password"
          value={key()}
          onInput={(e) => setKey(e.currentTarget.value)}
          autocomplete="off"
          required
        />
      </label>
      <label class="check-label">
        <input
          type="checkbox"
          checked={supports()}
          onChange={(e) => setSupports(e.currentTarget.checked)}
        />
        Provider confirmed 1m context
      </label>
      <Show when={error()}>
        <p class="form-error">{error()}</p>
      </Show>
      <button class="primary-button" type="submit">
        <Plus size={16} />
        Save provider
      </button>
    </form>
  );
}
function ModelForm(props: {
  providers: { id: string; display_name: string }[];
  done: () => Promise<void>;
}) {
  const [name, setName] = createSignal("");
  const [provider, setProvider] = createSignal(props.providers[0]?.id ?? "");
  const [upstream, setUpstream] = createSignal("");
  const [context, setContext] = createSignal<"200k" | "1m">("200k");
  const [images, setImages] = createSignal(false);
  const [tools, setTools] = createSignal(true);
  const [tokens, setTokens] = createSignal(4096);
  const [error, setError] = createSignal("");
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    try {
      await createModel({
        display_name: name(),
        provider_id: provider(),
        upstream_model_id: upstream(),
        context_window: context(),
        supports_images: images(),
        supports_tools: tools(),
        max_output_tokens: tokens(),
        enabled: true,
      });
      await props.done();
    } catch (value) {
      setError(value instanceof Error ? value.message : "Model could not be saved");
    }
  }
  return (
    <form class="inline-form" onSubmit={submit}>
      <label>
        Name
        <input value={name()} onInput={(e) => setName(e.currentTarget.value)} required />
      </label>
      <label>
        Provider
        <select value={provider()} onChange={(e) => setProvider(e.currentTarget.value)}>
          <For each={props.providers}>
            {(item) => <option value={item.id}>{item.display_name}</option>}
          </For>
        </select>
      </label>
      <label>
        Upstream model ID
        <input value={upstream()} onInput={(e) => setUpstream(e.currentTarget.value)} required />
      </label>
      <label>
        Context
        <select
          value={context()}
          onChange={(e) => setContext(e.currentTarget.value as "200k" | "1m")}
        >
          <option value="200k">200k</option>
          <option value="1m">1m</option>
        </select>
      </label>
      <label>
        Max output tokens
        <input
          type="number"
          min="1"
          value={tokens()}
          onInput={(e) => setTokens(Number(e.currentTarget.value))}
        />
      </label>
      <label class="check-label">
        <input
          type="checkbox"
          checked={images()}
          onChange={(e) => setImages(e.currentTarget.checked)}
        />
        Images
      </label>
      <label class="check-label">
        <input
          type="checkbox"
          checked={tools()}
          onChange={(e) => setTools(e.currentTarget.checked)}
        />
        Tools
      </label>
      <Show when={error()}>
        <p class="form-error">{error()}</p>
      </Show>
      <button class="primary-button" type="submit">
        <Plus size={16} />
        Save model
      </button>
    </form>
  );
}
