import { ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { Navigate, useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/button";
import { Input } from "../../components/ui/input";
import { useAuthStatus } from "./hooks/useAuthStatus";
import { useUpdateOwnerCredentials } from "./hooks/useUpdateOwnerCredentials";
import { AuthSurface, Field } from "./LoginPage";

export function SetupCredentialsPage() {
  const navigate = useNavigate();
  const authStatus = useAuthStatus();
  const updateCredentials = useUpdateOwnerCredentials();
  const [currentPassword, setCurrentPassword] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  useEffect(() => {
    if (authStatus.data?.user?.username) {
      setUsername(authStatus.data.user.username);
    }
  }, [authStatus.data?.user?.username]);

  if (authStatus.data?.authenticated === false) {
    return <Navigate to="/login" replace />;
  }

  if (
    authStatus.data?.authenticated === true &&
    authStatus.data.user?.requiresCredentialSetup !== true
  ) {
    return <Navigate to="/" replace />;
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    await updateCredentials.mutateAsync({
      currentPassword,
      username: username.trim(),
      password,
    });
    navigate("/", { replace: true });
  };

  return (
    <AuthSurface>
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-md rounded-lg border border-border bg-card p-5 shadow-sm"
      >
        <div className="mb-5 flex items-center gap-2">
          <div className="flex h-9 w-9 items-center justify-center rounded-xs bg-info-soft text-foreground">
            <ShieldCheck className="h-4 w-4" />
          </div>
          <div>
            <h1 className="text-base font-semibold text-foreground">
              Update credentials
            </h1>
            <p className="text-sm text-muted-foreground">
              Change the bootstrap username and password
            </p>
          </div>
        </div>

        <div className="space-y-3">
          <Field id="setup-current-password" label="Current password">
            <Input
              id="setup-current-password"
              type="password"
              autoComplete="current-password"
              value={currentPassword}
              onChange={(event) => setCurrentPassword(event.target.value)}
              required
            />
          </Field>
          <Field id="setup-username" label="New username">
            <Input
              id="setup-username"
              autoComplete="username"
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              required
            />
          </Field>
          <Field id="setup-password" label="New password">
            <Input
              id="setup-password"
              type="password"
              autoComplete="new-password"
              minLength={12}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              required
            />
          </Field>
        </div>

        {updateCredentials.isError && (
          <p className="mt-3 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            Credential update failed.
          </p>
        )}

        <Button
          type="submit"
          className="mt-5 w-full"
          disabled={updateCredentials.isPending}
        >
          {updateCredentials.isPending ? "Saving..." : "Save credentials"}
        </Button>
      </form>
    </AuthSurface>
  );
}
