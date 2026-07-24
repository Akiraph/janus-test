import { useQueryClient } from "@tanstack/solid-query";
import KeyRound from "lucide-solid/icons/key-round";
import LogOut from "lucide-solid/icons/log-out";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import ShieldCheck from "lucide-solid/icons/shield-check";
import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { useNotifications } from "../../components/ui/notifications";
import { logout } from "../../lib/api";
import { useMe } from "../../lib/queries";

export function SecurityPage() {
  const me = useMe();
  const client = useQueryClient();
  const notify = useNotifications().notify;

  async function signOut() {
    await logout();
    notify("Signed out", { variant: "success" });
    await client.invalidateQueries({ queryKey: ["me"] });
  }

  return (
    <div class="panel animate-panel-in">
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
          <Badge variant="success">
            <ShieldCheck size={12} />
            Protected
          </Badge>
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
            onClick={() =>
              notify("Recovery code regeneration is available from the authenticated API.", {
                variant: "info",
              })
            }
          >
            Manage
          </Button>
        </div>

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
