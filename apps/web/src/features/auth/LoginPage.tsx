import { LockKeyhole } from "lucide-react";
import { useState } from "react";
import { Navigate, useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/button";
import { Input } from "../../components/ui/input";
import { useAuthStatus } from "./hooks/useAuthStatus";
import { useLogin } from "./hooks/useLogin";

export function LoginPage() {
  const navigate = useNavigate();
  const authStatus = useAuthStatus();
  const loginMutation = useLogin();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  if (authStatus.data?.authenticated) {
    return (
      <Navigate
        to={
          authStatus.data.user?.requiresCredentialSetup === true
            ? "/setup"
            : "/"
        }
        replace
      />
    );
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    const response = await loginMutation.mutateAsync({
      username: username.trim(),
      password,
    });
    navigate(response.user?.requiresCredentialSetup === true ? "/setup" : "/", {
      replace: true,
    });
  };

  return (
    <AuthSurface>
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-sm rounded-lg border border-border bg-card p-5 shadow-sm"
      >
        <div className="mb-5 flex items-center gap-2">
          <div className="flex h-9 w-9 items-center justify-center rounded-xs bg-info-soft text-foreground">
            <LockKeyhole className="h-4 w-4" />
          </div>
          <div>
            <h1 className="text-base font-semibold text-foreground">Sign in</h1>
            <p className="text-sm text-muted-foreground">Janus local owner</p>
          </div>
        </div>

        <div className="space-y-3">
          <Field id="login-username" label="Username">
            <Input
              id="login-username"
              autoComplete="username"
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              required
            />
          </Field>
          <Field id="login-password" label="Password">
            <Input
              id="login-password"
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              required
            />
          </Field>
        </div>

        {loginMutation.isError && (
          <p className="mt-3 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            Sign in failed.
          </p>
        )}

        <Button
          type="submit"
          className="mt-5 w-full"
          disabled={loginMutation.isPending}
        >
          {loginMutation.isPending ? "Signing in..." : "Sign in"}
        </Button>
      </form>
    </AuthSurface>
  );
}

export function AuthSurface({
  children,
}: {
  readonly children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-full items-center justify-center bg-background p-4">
      {children}
    </div>
  );
}

export function Field({
  id,
  label,
  children,
}: {
  readonly id: string;
  readonly label: string;
  readonly children: React.ReactNode;
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
