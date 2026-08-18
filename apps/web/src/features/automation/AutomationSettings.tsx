import { A } from "@solidjs/router";
import { useQueryClient } from "@tanstack/solid-query";
import Bot from "lucide-solid/icons/bot";
import CheckCircle2 from "lucide-solid/icons/check-circle-2";
import Copy from "lucide-solid/icons/copy";
import ExternalLink from "lucide-solid/icons/external-link";
import Eye from "lucide-solid/icons/eye";
import EyeOff from "lucide-solid/icons/eye-off";
import GitPullRequest from "lucide-solid/icons/git-pull-request";
import KeyRound from "lucide-solid/icons/key-round";
import Loader2 from "lucide-solid/icons/loader-2";
import Pencil from "lucide-solid/icons/pencil";
import Plus from "lucide-solid/icons/plus";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Trash2 from "lucide-solid/icons/trash-2";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { EmptyState } from "../../components/ui/EmptyState";
import { useNotifications } from "../../components/ui/notifications";
import type {
  AutomationRunView,
  CreateGithubCredentialInput,
  GithubCredentialView,
} from "../../lib/api";
import {
  createGithubCredential,
  deleteGithubCredential,
  getAutomationWebhookConfig,
  getErrorMessage,
  probeGithubCredential,
  updateAutomationSettings,
  updateGithubCredential,
} from "../../lib/api";
import {
  useAutomationSettings,
  useAutomations,
  useAutomationWebhookConfig,
  useGithubCredentials,
  useProviders,
} from "../../lib/queries";
import "./automation.css";

export function AutomationSettings() {
  const queryClient = useQueryClient();
  const automations = useAutomations();
  const webhookConfig = useAutomationWebhookConfig();
  const automationSettings = useAutomationSettings();
  const providers = useProviders();
  const credentials = useGithubCredentials();
  const notify = useNotifications().notify;
  const [editing, setEditing] = createSignal<GithubCredentialView | null>(null);
  const [formOpen, setFormOpen] = createSignal(false);
  const [webhookSecret, setWebhookSecret] = createSignal<string | null>(null);
  const [secretVisible, setSecretVisible] = createSignal(false);
  const [modelSaving, setModelSaving] = createSignal(false);

  const modelOptions = () =>
    (providers.data ?? []).flatMap((provider) =>
      provider.client === "supervisor" && provider.enabled
        ? provider.models
            .filter((model) => model.enabled)
            .map((model) => ({
              providerId: provider.id,
              upstreamId: model.upstream_model_id,
              label: `${provider.display_name} / ${model.display_name}`,
            }))
        : [],
    );

  const selectedModel = () => {
    const providerId = automationSettings.data?.model_provider_id;
    const upstreamId = automationSettings.data?.model_upstream_id;
    return providerId && upstreamId ? JSON.stringify([providerId, upstreamId]) : "";
  };

  async function saveModel(value: string) {
    const [providerId, upstreamId] = value ? JSON.parse(value) : [null, null];
    setModelSaving(true);
    try {
      await updateAutomationSettings({
        model_provider_id: providerId,
        model_upstream_id: upstreamId,
      });
      await queryClient.invalidateQueries({ queryKey: ["automation-settings"] });
      notify("Automation model saved", { variant: "success" });
    } catch (error) {
      notify(getErrorMessage(error, "Automation model could not be saved"), { variant: "danger" });
    } finally {
      setModelSaving(false);
    }
  }

  async function revealWebhookSecret() {
    try {
      const config = await getAutomationWebhookConfig(true);
      setWebhookSecret(config.secret ?? null);
      setSecretVisible(Boolean(config.secret));
    } catch (error) {
      notify(getErrorMessage(error, "Webhook secret could not be loaded"), { variant: "danger" });
    }
  }

  async function copyValue(value: string | undefined, label: string) {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      notify(`${label} copied`, { variant: "success" });
    } catch (error) {
      notify(getErrorMessage(error, `${label} could not be copied`), { variant: "danger" });
    }
  }

  async function refreshCredentials() {
    await queryClient.invalidateQueries({ queryKey: ["github-credentials"] });
  }

  async function removeCredential(credential: GithubCredentialView) {
    if (!confirm(`Delete credential "${credential.name}"?`)) return;
    try {
      await deleteGithubCredential(credential.id);
      notify("GitHub credential deleted", { variant: "success" });
      await refreshCredentials();
    } catch (error) {
      notify(getErrorMessage(error, "Credential deletion failed"), { variant: "danger" });
    }
  }

  async function probeCredential(credential: GithubCredentialView) {
    try {
      const result = await probeGithubCredential(credential.id);
      notify(`${credential.name}: ${result.detail}`, {
        variant: result.status === "ready" ? "success" : "danger",
        duration: result.status === "ready" ? 4000 : 0,
      });
    } catch (error) {
      notify(getErrorMessage(error, "Credential probe failed"), { variant: "danger" });
    }
  }

  return (
    <div class="panel automation-settings">
      <div class="panel-heading">
        <h2>Automation</h2>
        <p>Webhook-driven workflows with explicit, auditable push access.</p>
      </div>

      <section class="automation-config-callout" aria-labelledby="automation-config-title">
        <div>
          <h3 id="automation-config-title">Automation push access</h3>
          <p>
            Project GitHub credentials stay project-only unless you explicitly enable them here.
            Nothing is selected for Automation by default.
          </p>
        </div>
        <a class="automation-config-link" href="#automation-credentials-title">
          Configure push credentials
        </a>
      </section>

      <section class="automation-section" aria-labelledby="automation-model-title">
        <div class="automation-section__heading">
          <div>
            <h3 id="automation-model-title">Execution model</h3>
            <p>New Automation sessions use this model for their first turn.</p>
          </div>
        </div>
        <label class="automation-model-setting">
          <span>Model</span>
          <select
            class="ui-input"
            value={selectedModel()}
            disabled={modelSaving() || providers.isPending}
            onChange={(event) => void saveModel(event.currentTarget.value)}
          >
            <option value="">Use project/default model</option>
            <For each={modelOptions()}>
              {(model) => (
                <option value={JSON.stringify([model.providerId, model.upstreamId])}>
                  {model.label}
                </option>
              )}
            </For>
          </select>
          <small>
            {automationSettings.data?.model_display_name ?? "No Automation-specific model selected"}
          </small>
        </label>
      </section>

      <section class="automation-section" aria-labelledby="automation-runs-title">
        <div class="automation-section__heading">
          <div>
            <h3 id="automation-runs-title">
              <Bot size={16} /> Runs
            </h3>
            <p>Recent runs stay linked to their project and session.</p>
          </div>
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label="Refresh automation runs"
            title="Refresh runs"
            onClick={() => void queryClient.invalidateQueries({ queryKey: ["automations"] })}
          >
            <RefreshCw size={15} />
          </Button>
        </div>
        <Show when={!automations.isPending} fallback={<p class="surface-note">Loading runs...</p>}>
          <Show
            when={(automations.data?.length ?? 0) > 0}
            fallback={
              <EmptyState
                icon={GitPullRequest}
                title="No automation runs"
                description="A signed webhook will appear here after it starts a workflow."
              />
            }
          >
            <div class="automation-runs">
              <For each={automations.data}>{(run) => <AutomationRunCard run={run} />}</For>
            </div>
          </Show>
        </Show>
      </section>

      <section class="automation-section" aria-labelledby="automation-webhook-title">
        <div class="automation-section__heading">
          <div>
            <h3 id="automation-webhook-title">
              <GitPullRequest size={16} /> Webhook
            </h3>
            <p>Inbound trigger for final email HTML or the compatible JSON envelope.</p>
          </div>
          <code class="automation-endpoint">
            {webhookConfig.data?.endpoint ?? "/api/v1/automation/webhook"}
          </code>
        </div>
        <div class="automation-webhook-card">
          <div class="automation-webhook-row">
            <span>Endpoint</span>
            <code>{webhookConfig.data?.endpoint ?? "Loading..."}</code>
            <Button
              variant="ghost"
              size="sm"
              iconOnly
              aria-label="Copy inbound webhook endpoint"
              title="Copy endpoint"
              onClick={() => void copyValue(webhookConfig.data?.endpoint, "Webhook endpoint")}
            >
              <Copy size={15} />
            </Button>
          </div>
          <div class="automation-webhook-row">
            <span>Status</span>
            <strong>{webhookConfig.data?.enabled ? "Enabled" : "Disabled"}</strong>
          </div>
          <div class="automation-webhook-row">
            <span>Secret</span>
            <code>
              {secretVisible()
                ? (webhookSecret() ?? "Unavailable")
                : webhookConfig.data?.secret_configured
                  ? "Configured (hidden)"
                  : "Not configured"}
            </code>
            <div class="record-actions">
              <Show when={webhookConfig.data?.secret_configured}>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    if (secretVisible()) {
                      setSecretVisible(false);
                    } else {
                      void revealWebhookSecret();
                    }
                  }}
                >
                  <Show
                    when={secretVisible()}
                    fallback={
                      <>
                        <Eye size={14} /> Reveal
                      </>
                    }
                  >
                    <EyeOff size={14} /> Hide
                  </Show>
                </Button>
              </Show>
              <Show when={secretVisible() && webhookSecret()}>
                <Button
                  variant="ghost"
                  size="sm"
                  iconOnly
                  aria-label="Copy webhook secret"
                  title="Copy secret"
                  onClick={() => void copyValue(webhookSecret() ?? undefined, "Webhook secret")}
                >
                  <Copy size={15} />
                </Button>
              </Show>
            </div>
          </div>
          <small>
            Send <code>X-Janus-Webhook-Secret</code> with the request. This inbound webhook is
            separate from outbound email notifications.
          </small>
        </div>
      </section>

      <section class="automation-section" aria-labelledby="automation-credentials-title">
        <div class="automation-section__heading">
          <div>
            <h3 id="automation-credentials-title">
              <KeyRound size={16} /> GitHub credentials
            </h3>
            <p>
              Encrypted PATs can be used for private projects and, only after explicit opt-in, for
              Automation pushes. A single enabled credential is selected when a webhook omits an id.
            </p>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setEditing(null);
              setFormOpen(true);
            }}
          >
            <Plus size={15} /> Add credential
          </Button>
        </div>
        <Show
          when={!credentials.isPending}
          fallback={<p class="surface-note">Loading credentials...</p>}
        >
          <Show
            when={(credentials.data?.length ?? 0) > 0}
            fallback={
              <EmptyState
                icon={KeyRound}
                title="No GitHub credentials"
                description="Add a classic PAT to let an automation push to a private or forked repository."
              />
            }
          >
            <div class="automation-credentials">
              <For each={credentials.data}>
                {(credential) => (
                  <article class="automation-credential">
                    <div class="automation-credential__copy">
                      <strong>{credential.name}</strong>
                      <span>{credential.github_host}</span>
                      <small>
                        {credential.pat_is_set
                          ? `PAT ${credential.pat_fingerprint ?? "stored"}`
                          : "No PAT stored"}
                      </small>
                      <small class={credential.automation_enabled ? "automation-enabled" : ""}>
                        {credential.automation_enabled
                          ? "Automation push enabled"
                          : "Project access only"}
                      </small>
                    </div>
                    <div class="record-actions">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void probeCredential(credential)}
                      >
                        <CheckCircle2 size={14} /> Probe
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        iconOnly
                        aria-label={`Edit ${credential.name}`}
                        title="Edit credential"
                        onClick={() => {
                          setEditing(credential);
                          setFormOpen(true);
                        }}
                      >
                        <Pencil size={16} />
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        iconOnly
                        aria-label={`Delete ${credential.name}`}
                        title="Delete credential"
                        onClick={() => void removeCredential(credential)}
                      >
                        <Trash2 size={16} />
                      </Button>
                    </div>
                  </article>
                )}
              </For>
            </div>
          </Show>
        </Show>
      </section>

      <Show when={formOpen()}>
        <GithubCredentialForm
          credential={editing()}
          close={() => setFormOpen(false)}
          saved={async () => {
            setFormOpen(false);
            await refreshCredentials();
          }}
        />
      </Show>
    </div>
  );
}

function AutomationRunCard(props: { run: AutomationRunView }) {
  const run = () => props.run;
  const status = () => run().operation.status;
  return (
    <article class="automation-run">
      <div class="automation-run__main">
        <div class="automation-run__title">
          <strong>{run().workflow}</strong>
          <span class={`automation-status automation-status--${status()}`}>{status()}</span>
          <span class="record-chip">{run().push_status}</span>
        </div>
        <div class="automation-run__meta">
          <span>Operation {run().operation.id}</span>
          <span>Source: {run().source}</span>
          <span>Step: {run().operation.current_step ?? "queued"}</span>
          <span>{formatDate(run().operation.updated_at)}</span>
        </div>
        <div class="automation-run__links">
          <Show when={run().pull_request_url}>
            {(url) => (
              <a href={url()} target="_blank" rel="noreferrer">
                PR <ExternalLink size={13} />
              </a>
            )}
          </Show>
          <Show
            when={(run().repositories ?? []).length > 0}
            fallback={
              <>
                <Show when={run().project_id}>
                  {(projectId) => <A href={`/projects/${projectId()}`}>Project {projectId()}</A>}
                </Show>
                <Show when={run().session_id}>
                  {(sessionId) => (
                    <Show when={run().project_id}>
                      {(projectId) => (
                        <A href={`/projects/${projectId()}/sessions/${sessionId()}`}>
                          Session {sessionId()}
                        </A>
                      )}
                    </Show>
                  )}
                </Show>
              </>
            }
          >
            <div class="automation-run__repositories">
              <For each={run().repositories ?? []}>
                {(repository) => (
                  <div class="automation-run__repository">
                    <a href={repository.repository_url} target="_blank" rel="noreferrer">
                      {repository.repository_url.replace(/^https?:\/\//, "")}
                      <ExternalLink size={13} />
                    </a>
                    <span class={`automation-status automation-status--${repository.status}`}>
                      {repository.status}
                    </span>
                    <Show when={repository.project_id}>
                      {(projectId) => <A href={`/projects/${projectId()}`}>Project</A>}
                    </Show>
                    <Show when={repository.project_id && repository.session_id}>
                      <A
                        href={`/projects/${repository.project_id}/sessions/${repository.session_id}`}
                      >
                        Session
                      </A>
                    </Show>
                    <Show when={repository.detail}>
                      {(detail) => <span class="muted">{detail()}</span>}
                    </Show>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
    </article>
  );
}

interface GithubCredentialFormProps {
  credential: GithubCredentialView | null;
  close: () => void;
  saved: () => Promise<void>;
}

function GithubCredentialForm(props: GithubCredentialFormProps) {
  const notify = useNotifications().notify;
  const editing = () => props.credential !== null;
  const [name, setName] = createSignal(props.credential?.name ?? "");
  const [host, setHost] = createSignal(props.credential?.github_host ?? "github.com");
  const [pat, setPat] = createSignal("");
  const [automationEnabled, setAutomationEnabled] = createSignal(
    props.credential?.automation_enabled ?? false,
  );
  const [error, setError] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    const cleanName = name().trim();
    const cleanHost = host().trim();
    const cleanPat = pat().trim();
    if (!cleanName || !cleanHost || (!editing() && !cleanPat)) {
      setError("Name, GitHub host, and a PAT are required");
      return;
    }
    setSubmitting(true);
    setError("");
    try {
      if (props.credential) {
        const input: {
          name: string;
          github_host: string;
          pat?: string;
          automation_enabled: boolean;
        } = {
          name: cleanName,
          github_host: cleanHost,
          automation_enabled: automationEnabled(),
        };
        if (cleanPat) input.pat = cleanPat;
        await updateGithubCredential(props.credential.id, props.credential.version, input);
        notify("GitHub credential updated", { variant: "success" });
      } else {
        const input: CreateGithubCredentialInput = {
          name: cleanName,
          github_host: cleanHost,
          pat: cleanPat,
          automation_enabled: automationEnabled(),
        };
        await createGithubCredential(input);
        notify("GitHub credential added", { variant: "success" });
      }
      await props.saved();
    } catch (value) {
      setError(getErrorMessage(value, "Credential could not be saved"));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog
      title={editing() ? "Edit GitHub credential" : "Add GitHub credential"}
      description="The PAT is encrypted at rest and never displayed after saving."
      close={props.close}
    >
      <form class="dialog-form" onSubmit={submit}>
        <div class="dialog-form-grid">
          <div>
            <span class="field-label">Name</span>
            <input
              class="ui-input"
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              required
            />
          </div>
          <div>
            <span class="field-label">GitHub host</span>
            <input
              class="ui-input"
              value={host()}
              onInput={(event) => setHost(event.currentTarget.value)}
              required
            />
          </div>
          <div class="full-field">
            <span class="field-label">Classic PAT</span>
            <input
              class="ui-input"
              type="password"
              value={pat()}
              onInput={(event) => setPat(event.currentTarget.value)}
              placeholder={editing() ? "Leave blank to keep the stored PAT" : "ghp_..."}
              autocomplete="new-password"
              required={!editing()}
            />
          </div>
          <label class="full-field automation-credential-toggle">
            <input
              type="checkbox"
              checked={automationEnabled()}
              onChange={(event) => setAutomationEnabled(event.currentTarget.checked)}
            />
            <span>
              <strong>Allow Automation pushes</strong>
              <small>
                This PAT may be used by an AI workflow to clone, repair, commit, and push.
              </small>
            </span>
          </label>
        </div>
        <Show when={error()}>
          <p class="form-error">{error()}</p>
        </Show>
        <div class="dialog-footer">
          <Button variant="outline" type="button" onClick={props.close}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={submitting()}>
            <Show when={submitting()} fallback={editing() ? "Save changes" : "Add credential"}>
              <Loader2 size={15} class="ui-spinner" /> Saving...
            </Show>
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}
