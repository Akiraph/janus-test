import { Check, Pencil, X } from "lucide-react";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";
import { Button } from "../../components/ui/button";
import { Dialog, DialogFooter } from "../../components/ui/dialog";
import { Input } from "../../components/ui/input";
import { useAuthStatus } from "../auth/hooks/useAuthStatus";
import { useUpdateOwnerPassword } from "../auth/hooks/useUpdateOwnerPassword";
import { useUpdateOwnerUsername } from "../auth/hooks/useUpdateOwnerUsername";

export function SecurityPanel() {
  const authStatus = useAuthStatus();
  const updateUsername = useUpdateOwnerUsername();
  const updatePassword = useUpdateOwnerPassword();
  const currentUsername = authStatus.data?.user?.username ?? "";
  const [isEditingUsername, setIsEditingUsername] = useState(false);
  const [usernameDraft, setUsernameDraft] = useState(currentUsername);
  const [isPasswordDialogOpen, setIsPasswordDialogOpen] = useState(false);
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [passwordError, setPasswordError] = useState<string | undefined>();

  useEffect(() => {
    if (!isEditingUsername) {
      setUsernameDraft(currentUsername);
    }
  }, [currentUsername, isEditingUsername]);

  const handleStartUsernameEdit = () => {
    updateUsername.reset();
    setUsernameDraft(currentUsername);
    setIsEditingUsername(true);
  };

  const handleCancelUsernameEdit = () => {
    setUsernameDraft(currentUsername);
    setIsEditingUsername(false);
  };

  const handleUsernameSubmit = async (event: FormEvent) => {
    event.preventDefault();
    const username = usernameDraft.trim();

    if (username === currentUsername) {
      setIsEditingUsername(false);
      return;
    }

    await updateUsername.mutateAsync({ username });
    setIsEditingUsername(false);
  };

  const handlePasswordDialogOpenChange = (open: boolean) => {
    setIsPasswordDialogOpen(open);

    if (!open) {
      setCurrentPassword("");
      setNewPassword("");
      setConfirmPassword("");
      setPasswordError(undefined);
      updatePassword.reset();
    }
  };

  const handlePasswordSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setPasswordError(undefined);

    if (newPassword !== confirmPassword) {
      setPasswordError("New passwords do not match.");
      return;
    }

    await updatePassword.mutateAsync({
      currentPassword,
      password: newPassword,
    });
    handlePasswordDialogOpenChange(false);
  };

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold text-foreground">Security</h2>
        <p className="text-sm text-muted-foreground">Local owner account</p>
      </div>

      <div className="space-y-3">
        <AccountRow label="Username">
          {isEditingUsername ? (
            <form
              onSubmit={handleUsernameSubmit}
              className="flex min-w-0 flex-1 items-center justify-end gap-2"
            >
              <Input
                id="security-username"
                className="h-8 max-w-64"
                autoComplete="username"
                minLength={3}
                maxLength={64}
                pattern="[A-Za-z0-9][A-Za-z0-9._-]*"
                value={usernameDraft}
                onChange={(event) => setUsernameDraft(event.target.value)}
                required
                autoFocus
              />
              <Button
                type="submit"
                size="icon-sm"
                aria-label="Save username"
                disabled={updateUsername.isPending}
              >
                <Check className="h-4 w-4" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label="Cancel username edit"
                onClick={handleCancelUsernameEdit}
                disabled={updateUsername.isPending}
              >
                <X className="h-4 w-4" />
              </Button>
            </form>
          ) : (
            <div className="flex min-w-0 flex-1 items-center justify-end gap-2">
              <span className="truncate text-sm text-foreground">
                {currentUsername}
              </span>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label="Edit username"
                onClick={handleStartUsernameEdit}
                disabled={!currentUsername}
              >
                <Pencil className="h-4 w-4" />
              </Button>
            </div>
          )}
        </AccountRow>

        <AccountRow label="Password">
          <Button
            type="button"
            variant="outline"
            onClick={() => setIsPasswordDialogOpen(true)}
          >
            Change
          </Button>
        </AccountRow>
      </div>

      {updateUsername.isError && (
        <p className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
          Username update failed.
        </p>
      )}

      <Dialog
        open={isPasswordDialogOpen}
        onOpenChange={handlePasswordDialogOpenChange}
        title="Change password"
        description="Enter your current password and choose a new password."
      >
        <form onSubmit={handlePasswordSubmit} className="space-y-4">
          <Field id="security-current-password" label="Current password">
            <Input
              id="security-current-password"
              type="password"
              autoComplete="current-password"
              value={currentPassword}
              onChange={(event) => setCurrentPassword(event.target.value)}
              required
            />
          </Field>
          <Field id="security-new-password" label="New password">
            <Input
              id="security-new-password"
              type="password"
              autoComplete="new-password"
              minLength={12}
              maxLength={256}
              value={newPassword}
              onChange={(event) => setNewPassword(event.target.value)}
              required
            />
          </Field>
          <Field id="security-confirm-password" label="Confirm password">
            <Input
              id="security-confirm-password"
              type="password"
              autoComplete="new-password"
              minLength={12}
              maxLength={256}
              value={confirmPassword}
              onChange={(event) => setConfirmPassword(event.target.value)}
              required
            />
          </Field>

          {(passwordError !== undefined || updatePassword.isError) && (
            <p className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
              {passwordError ?? "Password update failed."}
            </p>
          )}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => handlePasswordDialogOpenChange(false)}
              disabled={updatePassword.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={updatePassword.isPending}>
              {updatePassword.isPending ? "Saving..." : "Save"}
            </Button>
          </DialogFooter>
        </form>
      </Dialog>
    </div>
  );
}

function AccountRow({
  label,
  children,
}: {
  readonly label: string;
  readonly children: ReactNode;
}) {
  return (
    <div className="flex min-h-14 items-center justify-between gap-4 rounded-md border border-border bg-card px-4 py-3">
      <div className="min-w-0">
        <p className="text-sm font-medium text-foreground">{label}</p>
      </div>
      {children}
    </div>
  );
}

function Field({
  id,
  label,
  children,
}: {
  readonly id: string;
  readonly label: string;
  readonly children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={id} className="text-sm font-medium text-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}
