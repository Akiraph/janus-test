import { useQueryClient } from "@tanstack/solid-query";
import Copy from "lucide-solid/icons/copy";
import KeyRound from "lucide-solid/icons/key-round";
import LockKeyhole from "lucide-solid/icons/lock-keyhole";
import ShieldCheck from "lucide-solid/icons/shield-check";
import type { Component } from "solid-js";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { NotificationEvent } from "../../components/ui/notifications";
import type { TotpProvision } from "../../lib/api";
import {
  getErrorMessage,
  initializeComplete,
  initializeOptions,
  loginComplete,
  loginOptions,
  recoveryComplete,
  recoveryExchange,
  recoveryOptions,
  totpInitializeComplete,
  totpInitializeOptions,
  totpLogin,
} from "../../lib/api";
import { authenticationOptions, credentialPayload, registrationOptions } from "../../lib/webauthn";
import "./auth.css";

const passkeysSupported = "PublicKeyCredential" in window && "credentials" in navigator;

// Browser WebAuthn failures surface as DOMExceptions whose messages read as spec
// prose; map the ones an owner can act on.
const WEBAUTHN_MESSAGES: Record<string, string> = {
  NotAllowedError: "The passkey prompt was dismissed or timed out. Try again.",
  InvalidStateError: "This device already holds a passkey for Janus. Sign in with it instead.",
  NotSupportedError: "This browser cannot create the requested passkey.",
  SecurityError: "Passkeys need Janus served over HTTPS or on localhost.",
  AbortError: "The passkey request was cancelled.",
};

function authErrorMessage(value: unknown, fallback: string): string {
  if (value instanceof DOMException) {
    const message = WEBAUTHN_MESSAGES[value.name];
    if (message) return message;
  }
  return getErrorMessage(value, fallback);
}

export function SetupView() {
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
      setError(authErrorMessage(value, "Initialization failed."));
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
          <Show when={!passkeysSupported}>
            <p class="auth-note" role="alert">
              This browser cannot use passkeys. Try a current browser over HTTPS or on localhost.
            </p>
          </Show>
          <NotificationEvent message={error()} variant="danger" />
          <Button
            variant="primary"
            class="auth-submit"
            type="submit"
            disabled={busy() || !passkeysSupported}
          >
            <KeyRound size={17} aria-hidden="true" />
            {busy() ? "Waiting for passkey..." : "Create owner passkey"}
          </Button>
        </form>
      </Show>
    </AuthSurface>
  );
}

export function TotpSetupView() {
  const queryClient = useQueryClient();
  const [token, setToken] = createSignal("");
  const [name, setName] = createSignal("Owner");
  const [provision, setProvision] = createSignal<TotpProvision | null>(null);
  const [code, setCode] = createSignal("");
  const [codes, setCodes] = createSignal<string[]>([]);
  const [copyState, setCopyState] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      setProvision(await totpInitializeOptions(token(), name()));
    } catch (value) {
      setError(authErrorMessage(value, "Initialization failed."));
    } finally {
      setBusy(false);
    }
  }

  async function confirm(event: SubmitEvent) {
    event.preventDefault();
    const current = provision();
    if (!current) return;
    setBusy(true);
    setError("");
    try {
      const result = await totpInitializeComplete(current.ceremony_id, code());
      setCodes(result.recoveryCodes);
      await queryClient.invalidateQueries({ queryKey: ["bootstrap"] });
      await queryClient.invalidateQueries({ queryKey: ["me"] });
    } catch (value) {
      setError(authErrorMessage(value, "Initialization failed."));
    } finally {
      setBusy(false);
    }
  }

  async function copySecret() {
    const current = provision();
    if (!current) return;
    try {
      await navigator.clipboard.writeText(current.secret_base32);
      setCopyState("Secret key copied to your clipboard.");
    } catch {
      setCopyState("Copy failed. Select the key and copy it manually.");
    }
  }

  return (
    <AuthSurface
      icon={KeyRound}
      title="Initialize Janus"
      subtitle="Enroll an authenticator app for this deployment."
      wide
    >
      <Show
        when={codes().length === 0}
        fallback={<RecoveryCodes codes={codes()} done={() => location.reload()} />}
      >
        <Show
          when={provision()}
          fallback={
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
              <NotificationEvent message={error()} variant="danger" />
              <Button variant="primary" class="auth-submit" type="submit" disabled={busy()}>
                <KeyRound size={17} aria-hidden="true" />
                {busy() ? "Generating key..." : "Generate TOTP key"}
              </Button>
            </form>
          }
        >
          <form class="auth-form" onSubmit={confirm}>
            <div class="auth-secret">
              Secret key
              <code>{provision()?.secret_base32}</code>
            </div>
            <Button variant="outline" type="button" onClick={() => void copySecret()}>
              <Copy size={16} aria-hidden="true" />
              Copy secret
            </Button>
            <Show when={copyState()}>
              <p class="auth-note" role="status">
                {copyState()}
              </p>
            </Show>
            <p class="auth-note">
              Add this key to your authenticator app (e.g. Google Authenticator or 1Password),
              manually or by scanning the URI below if your app supports it.
            </p>
            <code>{provision()?.otpauth_uri}</code>
            <label>
              Confirmation code
              <input
                class="ui-input"
                inputmode="numeric"
                maxLength={6}
                value={code()}
                onInput={(event) => setCode(event.currentTarget.value)}
                autocomplete="off"
                required
              />
            </label>
            <NotificationEvent message={error()} variant="danger" />
            <Button variant="primary" class="auth-submit" type="submit" disabled={busy()}>
              <KeyRound size={17} aria-hidden="true" />
              {busy() ? "Confirming..." : "Confirm code"}
            </Button>
          </form>
        </Show>
      </Show>
    </AuthSurface>
  );
}

export function LoginView() {
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
      setError(authErrorMessage(value, "Login failed."));
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
      setError(authErrorMessage(value, "Recovery failed."));
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
        <NotificationEvent message={error()} variant="danger" />
        <Show when={!passkeysSupported}>
          <p class="auth-note" role="alert">
            This browser cannot use passkeys. Try a current browser over HTTPS or on localhost.
          </p>
        </Show>
        <Button
          variant="primary"
          class="auth-submit"
          type="button"
          onClick={() => void login()}
          disabled={busy() || !passkeysSupported}
        >
          <KeyRound size={17} aria-hidden="true" />
          {busy() ? "Waiting for passkey..." : "Continue with passkey"}
        </Button>
        <Button variant="ghost" type="button" onClick={() => setRecovery(!recovery())}>
          {recovery() ? "Use a passkey" : "Lost your passkey? Use a recovery code"}
        </Button>
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
            <Button
              variant="outline"
              class="auth-submit"
              type="submit"
              disabled={busy() || !passkeysSupported}
            >
              <KeyRound size={16} aria-hidden="true" />
              Bind a new passkey
            </Button>
          </form>
        </Show>
      </div>
    </AuthSurface>
  );
}

export function TotpLoginView() {
  const queryClient = useQueryClient();
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [recovery, setRecovery] = createSignal(false);

  async function login(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      await totpLogin(code());
      await queryClient.invalidateQueries({ queryKey: ["me"] });
    } catch (value) {
      setError(authErrorMessage(value, recovery() ? "Recovery failed." : "Login failed."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthSurface
      icon={LockKeyhole}
      title="Welcome back"
      subtitle="Enter the code from your authenticator app."
    >
      <form class="auth-form" onSubmit={login}>
        <label>
          {recovery() ? "One-time recovery code" : "6-digit code"}
          <input
            class="ui-input"
            inputmode={recovery() ? "text" : "numeric"}
            maxLength={recovery() ? 64 : 6}
            value={code()}
            onInput={(event) => setCode(event.currentTarget.value)}
            autocomplete="off"
            required
          />
        </label>
        <p class="auth-note">
          {recovery()
            ? "Enter one of your single-use recovery codes."
            : "Enter the 6-digit code from your authenticator app."}
        </p>
        <NotificationEvent message={error()} variant="danger" />
        <Button variant="primary" class="auth-submit" type="submit" disabled={busy()}>
          <KeyRound size={17} aria-hidden="true" />
          {busy() ? "Verifying..." : "Continue"}
        </Button>
        <Button
          variant="ghost"
          type="button"
          onClick={() => {
            setRecovery(!recovery());
            setCode("");
          }}
        >
          {recovery() ? "Enter a 6-digit code" : "Lost your authenticator? Use a recovery code"}
        </Button>
      </form>
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
  const [copyState, setCopyState] = createSignal("");

  async function copyCodes() {
    try {
      await navigator.clipboard.writeText(props.codes.join("\n"));
      setCopyState("Recovery codes copied to your clipboard.");
    } catch {
      setCopyState("Copy failed. Select the codes and copy them manually.");
    }
  }

  return (
    <div class="recovery-codes">
      <div class="recovery-heading">
        <ShieldCheck size={20} aria-hidden="true" />
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
      <Show when={copyState()}>
        <p class="auth-note" role="status">
          {copyState()}
        </p>
      </Show>
      <div class="recovery-actions">
        <Button variant="outline" type="button" onClick={() => void copyCodes()}>
          <Copy size={16} aria-hidden="true" />
          Copy codes
        </Button>
        <Button variant="primary" type="button" onClick={props.done}>
          I saved these codes
        </Button>
      </div>
    </div>
  );
}
