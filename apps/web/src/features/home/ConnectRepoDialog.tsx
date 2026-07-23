import { Loader2, Plus, X } from "lucide-react";
import { useState } from "react";
import { Button } from "../../components/ui/button";
import { Dialog } from "../../components/ui/dialog";
import { Input } from "../../components/ui/input";
import { Select } from "../../components/ui/select";
import { useConnectProject } from "./hooks/useConnectProject";
import { useCreateCredential } from "./hooks/useCreateCredential";
import { useCredentials } from "./hooks/useCredentials";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
};

export function ConnectRepoDialog({ open, onOpenChange }: Props) {
  const [owner, setOwner] = useState("");
  const [repo, setRepo] = useState("");
  const [selectedCredential, setSelectedCredential] = useState<string>("");
  const [showNewCredentialForm, setShowNewCredentialForm] = useState(false);
  const [newCredentialAlias, setNewCredentialAlias] = useState("");
  const [newCredentialPat, setNewCredentialPat] = useState("");

  const { data: credentialsData, isLoading: isLoadingCredentials } =
    useCredentials();
  const createCredential = useCreateCredential();
  const connectProject = useConnectProject();

  const credentials = credentialsData?.credentials ?? [];
  const githubCredentials = credentials.filter((c) => c.kind === "github_pat");

  // Create the credential in the vault on its own. On success the alias is
  // folded back into the selector so the user can see it landed, then the
  // sub-form collapses, and the main "Connect" action stays the single submit.
  const handleAddCredential = async () => {
    if (!newCredentialAlias || !newCredentialPat) return;
    try {
      const result = await createCredential.mutateAsync({
        alias: newCredentialAlias,
        kind: "github_pat",
        secret: newCredentialPat,
      });
      setSelectedCredential(result.credential.alias);
      setShowNewCredentialForm(false);
      setNewCredentialAlias("");
      setNewCredentialPat("");
    } catch {
      // Error is surfaced via createCredential.isError below.
    }
  };

  const handleConnect = async () => {
    try {
      await connectProject.mutateAsync({
        provider: "github",
        owner,
        repo,
        gitCredentialAlias: selectedCredential,
      });

      setOwner("");
      setRepo("");
      setSelectedCredential("");
      setShowNewCredentialForm(false);
      setNewCredentialAlias("");
      setNewCredentialPat("");
      onOpenChange(false);
    } catch {
      // Error is handled by TanStack Query and displayed in the UI
    }
  };

  const newCredentialReady = Boolean(newCredentialAlias && newCredentialPat);
  const isValid = Boolean(owner && repo && selectedCredential);

  const isConnecting = connectProject.isPending;
  const isAddingCredential = createCredential.isPending;

  const selectOptions = githubCredentials.map((cred) => ({
    value: cred.alias,
    label: cred.alias,
  }));

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title="Connect repository"
      description="Connect a GitHub repository to start working with Janus."
    >
      <div className="space-y-4">
        {/* Owner input */}
        <div className="space-y-2">
          <label
            htmlFor="owner"
            className="text-sm font-medium text-foreground"
          >
            Owner
          </label>
          <Input
            id="owner"
            placeholder="octocat"
            value={owner}
            onChange={(e) => setOwner(e.target.value)}
            disabled={isConnecting}
          />
        </div>

        {/* Repo input */}
        <div className="space-y-2">
          <label htmlFor="repo" className="text-sm font-medium text-foreground">
            Repository
          </label>
          <Input
            id="repo"
            placeholder="my-repo"
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            disabled={isConnecting}
          />
        </div>

        {/* Credential selector */}
        {!showNewCredentialForm && (
          <div className="space-y-2">
            <label
              htmlFor="credential"
              className="text-sm font-medium text-foreground"
            >
              Git credential
            </label>
            <Select
              options={selectOptions}
              value={selectedCredential}
              onValueChange={setSelectedCredential}
              placeholder={
                isLoadingCredentials
                  ? "Loading..."
                  : githubCredentials.length === 0
                    ? "No credentials configured"
                    : "Select a credential"
              }
              disabled={isLoadingCredentials || isConnecting}
            />
            <button
              type="button"
              onClick={() => setShowNewCredentialForm(true)}
              className="inline-flex items-center gap-1 text-sm text-foreground hover:underline disabled:opacity-60"
              disabled={isConnecting}
            >
              <Plus className="h-3.5 w-3.5" />
              Add a new credential
            </button>
          </div>
        )}

        {/* New credential form */}
        {showNewCredentialForm && (
          <div className="space-y-4 rounded-lg border border-border bg-muted/30 p-4">
            <div className="flex items-center justify-between">
              <h4 className="text-sm font-medium text-foreground">
                New credential
              </h4>
              <button
                type="button"
                onClick={() => setShowNewCredentialForm(false)}
                className="text-muted-foreground hover:text-foreground"
                disabled={isConnecting || isAddingCredential}
                aria-label="Close new credential form"
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <div className="space-y-2">
              <label
                htmlFor="new-alias"
                className="text-sm font-medium text-foreground"
              >
                Alias
              </label>
              <Input
                id="new-alias"
                placeholder="my-github-token"
                value={newCredentialAlias}
                onChange={(e) => setNewCredentialAlias(e.target.value)}
                disabled={isAddingCredential}
              />
            </div>

            <div className="space-y-2">
              <label
                htmlFor="new-pat"
                className="text-sm font-medium text-foreground"
              >
                Personal Access Token
              </label>
              <Input
                id="new-pat"
                type="password"
                placeholder="ghp_..."
                value={newCredentialPat}
                onChange={(e) => setNewCredentialPat(e.target.value)}
                disabled={isAddingCredential}
              />
            </div>

            <Button
              type="button"
              onClick={handleAddCredential}
              disabled={!newCredentialReady || isAddingCredential}
              className="w-full gap-1.5"
            >
              {isAddingCredential ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Plus className="h-3.5 w-3.5" />
              )}
              {isAddingCredential ? "Adding..." : "Add credential"}
            </Button>
          </div>
        )}

        {/* Error messages */}
        {createCredential.isError && (
          <div className="text-sm text-destructive">
            Failed to create credential. Please try again.
          </div>
        )}
        {connectProject.isError && (
          <div className="text-sm text-destructive">
            Failed to connect repository. Please check your inputs and try
            again.
          </div>
        )}

        {/* Actions */}
        <div className="flex justify-end gap-3 pt-2">
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isConnecting}
          >
            Cancel
          </Button>
          <Button onClick={handleConnect} disabled={!isValid || isConnecting}>
            {isConnecting ? "Connecting..." : "Connect"}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
