import type { ConfigureCredentialRequest } from "@janus/shared";
import { useState } from "react";
import { Button } from "../../components/ui/button";
import { Dialog } from "../../components/ui/dialog";
import { Input } from "../../components/ui/input";
import { useCreateCredential } from "../home/hooks/useCreateCredential";

export interface GitHubCredentialFormProps {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
}

/**
 * GitHubCredentialForm — Dialog for adding a new GitHub credential.
 * Collects alias and Personal Access Token.
 */
export function GitHubCredentialForm({
  open,
  onOpenChange,
}: GitHubCredentialFormProps) {
  const createMutation = useCreateCredential();
  const [alias, setAlias] = useState("");
  const [token, setToken] = useState("");

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    const request: ConfigureCredentialRequest = {
      alias: alias.trim(),
      kind: "github_pat",
      secret: token.trim(),
    };

    try {
      await createMutation.mutateAsync(request);
      // Reset form
      setAlias("");
      setToken("");
      onOpenChange(false);
    } catch {
      // Error is handled by TanStack Query and can be displayed in the UI
    }
  };

  const handleClose = () => {
    setAlias("");
    setToken("");
    onOpenChange(false);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={handleClose}
      title="Add GitHub Credential"
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div className="space-y-2">
          <label
            htmlFor="credential-alias"
            className="text-sm font-medium text-foreground"
          >
            Alias
          </label>
          <Input
            id="credential-alias"
            value={alias}
            onChange={(e) => setAlias(e.target.value)}
            placeholder="e.g., personal-github"
            required
          />
          <p className="text-xs text-muted-foreground">
            A friendly name to identify this credential
          </p>
        </div>

        <div className="space-y-2">
          <label
            htmlFor="credential-token"
            className="text-sm font-medium text-foreground"
          >
            Personal Access Token
          </label>
          <Input
            id="credential-token"
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder="ghp_..."
            required
          />
          <p className="text-xs text-muted-foreground">
            GitHub PAT with repository access
          </p>
        </div>

        <div className="flex justify-end gap-2 pt-4">
          <Button type="button" variant="outline" onClick={handleClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={createMutation.isPending}>
            {createMutation.isPending ? "Adding..." : "Add Credential"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
