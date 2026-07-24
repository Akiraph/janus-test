import { useQueryClient } from "@tanstack/solid-query";
import KeyRound from "lucide-solid/icons/key-round";
import LockKeyhole from "lucide-solid/icons/lock-keyhole";
import ShieldCheck from "lucide-solid/icons/shield-check";
import type { Component } from "solid-js";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { ErrorBlock } from "../../components/ui/ErrorBlock";
import {
  initializeComplete,
  initializeOptions,
  loginComplete,
  loginOptions,
  recoveryComplete,
  recoveryExchange,
  recoveryOptions,
} from "../../lib/api";
import { authenticationOptions, credentialPayload, registrationOptions } from "../../lib/webauthn";

export function SetupPage() {
  const queryClient = useQueryClient();
  const [token, setToken] = createSignal("");
  const [name, setName] = createSignal("Owner");
  const [codes, setCodes] = createSignal<string[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const options = await initializeOptions(token(), name());
      const credential = await navigator.credentials.create({
        publicKey: registrationOptions(options.public_key),
      });
      if (!(credential instanceof PublicKeyCredential))
        throw new Error("Passkey registration was cancelled.");
      const result = await initializeComplete(options.ceremony_id, credentialPayload(credential));
      setCodes(result.recoveryCodes);
      await queryClient.invalidateQueries({ queryKey: ["bootstrap"] });
      await queryClient.invalidateQueries({ queryKey: ["me"] });
    } catch (value) {
      setError(value instanceof Error ? value.message : "Initialization failed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthSurface
      icon={ShieldCheck}
      title="Initialize Janus"
      subtitle="Bind the first passkey to the deployment."
      wide
    >
      <Show
        when={codes().length === 0}
        fallback={<RecoveryCodes codes={codes()} done={() => location.reload()} />}
      >
        <form class="auth-form" onSubmit={submit}>
          <label>
            Display name
            <input
              class="ui-input"
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              required
            />
          </label>
          <label>
            Initialization token
            <input
              class="ui-input"
              type="password"
              value={token()}
              onInput={(event) => setToken(event.currentTarget.value)}
              autocomplete="off"
              required
            />
          </label>
          <Show when={error()}>
            <ErrorBlock variant="inline" message={error()} />
          </Show>
          <Button variant="primary" class="auth-submit" type="submit" disabled={busy()}>
            <KeyRound size={17} />
            {busy() ? "Waiting for passkey..." : "Create owner passkey"}
          </Button>
        </form>
      </Show>
    </AuthSurface>
  );
}

export function LoginPage() {
  const queryClient = useQueryClient();
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [recovery, setRecovery] = createSignal(false);
  const [code, setCode] = createSignal("");
  async function login() {
    setBusy(true);
    setError("");
    try {
      const options = await loginOptions();
      const credential = await navigator.credentials.get({
        publicKey: authenticationOptions(options.public_key),
      });
      if (!(credential instanceof PublicKeyCredential))
        throw new Error("Passkey login was cancelled.");
      await loginComplete(options.ceremony_id, credentialPayload(credential));
      await queryClient.invalidateQueries({ queryKey: ["me"] });
    } catch (value) {
      setError(value instanceof Error ? value.message : "Login failed.");
    } finally {
      setBusy(false);
    }
  }
  async function recover(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      await recoveryExchange(code());
      const options = await recoveryOptions("Recovered passkey");
      const credential = await navigator.credentials.create({
        publicKey: registrationOptions(options.public_key),
      });
      if (!(credential instanceof PublicKeyCredential))
        throw new Error("Passkey registration was cancelled.");
      await recoveryComplete(options.ceremony_id, credentialPayload(credential));
      await queryClient.invalidateQueries({ queryKey: ["me"] });
    } catch (value) {
      setError(value instanceof Error ? value.message : "Recovery failed.");
    } finally {
      setBusy(false);
    }
  }
  return (
    <AuthSurface
      icon={LockKeyhole}
      title="Welcome back"
      subtitle="Use a passkey to unlock your workspace."
    >
      <div class="auth-form">
        <Show when={error()}>
          <ErrorBlock variant="inline" message={error()} />
        </Show>
        <Button
          variant="primary"
          class="auth-submit"
          type="button"
          onClick={() => void login()}
          disabled={busy()}
        >
          <KeyRound size={17} />
          {busy() ? "Waiting for passkey..." : "Continue with passkey"}
        </Button>
        <button class="text-button" type="button" onClick={() => setRecovery(!recovery())}>
          {recovery() ? "Use a passkey" : "Lost your passkey? Use a recovery code"}
        </button>
        <Show when={recovery()}>
          <form class="auth-form" onSubmit={recover}>
            <label>
              One-time recovery code
              <input
                class="ui-input"
                value={code()}
                onInput={(event) => setCode(event.currentTarget.value)}
                autocomplete="off"
                required
              />
            </label>
            <Button variant="outline" class="auth-submit" type="submit" disabled={busy()}>
              <KeyRound size={16} />
              Bind a new passkey
            </Button>
          </form>
        </Show>
      </div>
    </AuthSurface>
  );
}

function AuthSurface(props: {
  icon: Component<{ size?: number }>;
  title: string;
  subtitle: string;
  wide?: boolean;
  children: unknown;
}) {
  return (
    <main class="auth-surface">
      <section class="auth-card" classList={{ wide: props.wide }}>
        <div class="auth-card-heading">
          <span class="auth-card-icon">
            <props.icon size={16} />
          </span>
          <div>
            <h1>{props.title}</h1>
            <p>{props.subtitle}</p>
          </div>
        </div>
        {props.children as never}
      </section>
    </main>
  );
}

function RecoveryCodes(props: { codes: string[]; done: () => void }) {
  return (
    <div class="recovery-codes">
      <div class="recovery-heading">
        <ShieldCheck size={20} />
        <div>
          <h2>Recovery codes</h2>
          <p>Store these now. Each code works once and will not be shown again.</p>
        </div>
      </div>
      <ol>
        <For each={props.codes}>
          {(code) => (
            <li>
              <code>{code}</code>
            </li>
          )}
        </For>
      </ol>
      <Button variant="primary" class="auth-submit" type="button" onClick={props.done}>
        I saved these codes
      </Button>
    </div>
  );
}
