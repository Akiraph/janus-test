import { useQueryClient } from "@tanstack/solid-query";
import KeyRound from "lucide-solid/icons/key-round";
import LogOut from "lucide-solid/icons/log-out";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
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

  async function signOut() {
    await logout();
    notify("Signed out", { variant: "success" });
    await client.invalidateQueries({ queryKey: ["me"] });
  }

  async function generateRecoveryCodes() {
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
            <KeyRound size={16} />
            <div>
              <strong>Passkeys</strong>
              <span>Use your device authenticator for every sign-in.</span>
            </div>
          </div>
        </div>

        <div class="account-row">
          <div class="account-label-with-icon">
            <RefreshCw size={16} />
            <div>
              <strong>Recovery codes</strong>
              <span>Generate a new one-time set from the recovery workflow.</span>
            </div>
          </div>
          <Button
            variant="outline"
            disabled={generating()}
            onClick={() => void generateRecoveryCodes()}
          >
            {generating() ? "Generating..." : "Generate new set"}
          </Button>
        </div>
        <Show when={codes().length > 0}>
          <div class="security-recovery-codes" role="status">
            <strong>Save these codes now</strong>
            <span>Each code works once. Generating another set revokes this set.</span>
            <ol>
              <For each={codes()}>
                {(code) => (
                  <li>
                    <code>{code}</code>
                  </li>
                )}
              </For>
            </ol>
            <Button variant="outline" onClick={() => setCodes([])}>
              Hide codes
            </Button>
          </div>
        </Show>

        <div class="account-row">
          <div class="account-label-with-icon">
            <LogOut size={16} />
            <div>
              <strong>Session</strong>
              <span>Sign out on this device.</span>
            </div>
          </div>
          <Button variant="destructive" onClick={() => void signOut()}>
            Sign out
          </Button>
        </div>
      </div>
    </div>
  );
}
