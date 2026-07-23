import { useQueryClient } from "@tanstack/solid-query";
import { KeyRound, RefreshCw, ShieldCheck } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import { logout } from "../../lib/api";
import { useMe } from "../../lib/queries";

export function SecurityPage() {
  const me = useMe();
  const client = useQueryClient();
  const [notice, setNotice] = createSignal("");
  async function signOut() {
    await logout();
    setNotice("Signed out");
    await client.invalidateQueries({ queryKey: ["me"] });
  }
  return (
    <div class="settings-page route-enter">
      <div class="page-heading">
        <div>
          <p class="eyebrow">Access</p>
          <h1>Security</h1>
          <p class="page-subtitle">Passkey-only access for this deployment.</p>
        </div>
      </div>
      <section class="settings-section">
        <div class="security-intro">
          <span class="auth-icon small">
            <ShieldCheck size={19} />
          </span>
          <div>
            <h2>{me.data?.data.display_name ?? "Owner"}</h2>
            <p>Authentication mode: {me.data?.data.authentication_mode ?? "passkey"}</p>
          </div>
        </div>
        <div class="settings-list">
          <div class="settings-row">
            <div class="row-icon">
              <KeyRound size={16} />
            </div>
            <div class="row-copy">
              <strong>Passkeys</strong>
              <span>Use your device authenticator for every sign-in.</span>
            </div>
            <span class="status-chip success">Protected</span>
          </div>
          <div class="settings-row">
            <div class="row-icon">
              <RefreshCw size={16} />
            </div>
            <div class="row-copy">
              <strong>Recovery codes</strong>
              <span>Generate a new one-time set from the recovery workflow.</span>
            </div>
            <button
              class="secondary-button"
              type="button"
              onClick={() =>
                setNotice("Recovery code regeneration is available from the authenticated API.")
              }
            >
              Manage
            </button>
          </div>
        </div>
        <Show when={notice()}>
          <p class="notice" role="status">
            {notice()}
          </p>
        </Show>
        <button class="danger-action" type="button" onClick={() => void signOut()}>
          Sign out
        </button>
      </section>
    </div>
  );
}
