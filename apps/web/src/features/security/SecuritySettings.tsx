import { useQueryClient } from "@tanstack/solid-query";
import Copy from "lucide-solid/icons/copy";
import KeyRound from "lucide-solid/icons/key-round";
import LogOut from "lucide-solid/icons/log-out";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { useNotifications } from "../../components/ui/notifications";
import { getErrorMessage, logout, regenerateRecoveryCodes } from "../../lib/api";
import { useMe } from "../../lib/queries";
import "./security.css";

export function SecuritySettings() {
  const me = useMe();
  const client = useQueryClient();
  const notify = useNotifications().notify;
  const [codes, setCodes] = createSignal<string[]>([]);
  const [generating, setGenerating] = createSignal(false);
  const [confirmingRegenerate, setConfirmingRegenerate] = createSignal(false);
  const [signingOut, setSigningOut] = createSignal(false);

  async function signOut() {
    setSigningOut(true);
    try {
      await logout();
      notify("Signed out", { variant: "success" });
      await client.invalidateQueries({ queryKey: ["me"] });
    } catch (error) {
      notify(getErrorMessage(error, "Sign out failed."), { variant: "danger" });
    } finally {
      setSigningOut(false);
    }
  }

  async function generateRecoveryCodes() {
    setConfirmingRegenerate(false);
    setGenerating(true);
    try {
      setCodes(await regenerateRecoveryCodes());
      notify("New recovery codes generated. Save them now.", { variant: "success", duration: 0 });
    } catch (error) {
      notify(getErrorMessage(error, "Recovery code regeneration failed."), { variant: "danger" });
    } finally {
      setGenerating(false);
    }
  }

  async function copyCodes() {
    try {
      await navigator.clipboard.writeText(codes().join("\n"));
      notify("Recovery codes copied", { variant: "success" });
    } catch (error) {
      notify(getErrorMessage(error, "Recovery codes could not be copied"), { variant: "danger" });
    }
  }

  return (
    <div class="panel">
      <div class="panel-heading">
        <h2>Security</h2>
        <p>Local owner account</p>
      </div>

      <div class="account-rows">
        <div class="account-row">
          <div>
            <strong>Owner</strong>
            <span>Display name for this deployment</span>
          </div>
          <span class="account-value">{me.data?.data.display_name ?? "Owner"}</span>
        </div>

        <div class="account-row">
          <div class="account-label-with-icon">
            <KeyRound size={16} aria-hidden="true" />
            <div>
              <strong>Passkeys</strong>
              <span>Use your device authenticator for every sign-in.</span>
            </div>
          </div>
        </div>

        <div class="account-row">
          <div class="account-label-with-icon">
            <RefreshCw size={16} aria-hidden="true" />
            <div>
              <strong>Recovery codes</strong>
              <span>Generate a new one-time set from the recovery workflow.</span>
            </div>
          </div>
          <Button
            variant="outline"
            disabled={generating()}
            onClick={() => setConfirmingRegenerate(true)}
          >
            {generating() ? "Generating..." : "Generate new set"}
          </Button>
        </div>
        <Show when={codes().length > 0}>
          <div class="security-recovery-codes" role="status">
            <strong>Save these codes now</strong>
            <span>
              Each code works once and is shown only here. Generating a new set revokes them.
            </span>
            <ol>
              <For each={codes()}>
                {(code) => (
                  <li>
                    <code>{code}</code>
                  </li>
                )}
              </For>
            </ol>
            <div class="security-recovery-actions">
              <Button variant="outline" onClick={() => void copyCodes()}>
                <Copy size={15} aria-hidden="true" /> Copy codes
              </Button>
              <Button variant="outline" onClick={() => setCodes([])}>
                Hide codes
              </Button>
            </div>
          </div>
        </Show>

        <div class="account-row">
          <div class="account-label-with-icon">
            <LogOut size={16} aria-hidden="true" />
            <div>
              <strong>Session</strong>
              <span>Sign out on this device.</span>
            </div>
          </div>
          <Button variant="destructive" disabled={signingOut()} onClick={() => void signOut()}>
            {signingOut() ? "Signing out..." : "Sign out"}
          </Button>
        </div>
      </div>

      <Show when={confirmingRegenerate()}>
        <Dialog
          title="Generate new recovery codes"
          description="The existing codes stop working immediately and cannot be recovered. The new set is shown once — save it before leaving this page."
          close={() => setConfirmingRegenerate(false)}
        >
          <div class="dialog-footer">
            <Button variant="outline" onClick={() => setConfirmingRegenerate(false)}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={() => void generateRecoveryCodes()}>
              Replace recovery codes
            </Button>
          </div>
        </Dialog>
      </Show>
    </div>
  );
}
