import { GitBranch, Plus } from "lucide-react";
import { useState } from "react";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { EmptyState } from "../../components/ui/empty-state";
import { Skeleton } from "../../components/ui/skeleton";
import { useCredentials } from "../home/hooks/useCredentials";
import { GitHubCredentialForm } from "./GitHubCredentialForm";

export function GitHubCredentialsPanel() {
  const { data: credentialsData, isLoading } = useCredentials();
  const [formOpen, setFormOpen] = useState(false);

  const handleAddCredential = () => {
    setFormOpen(true);
  };

  if (isLoading) {
    return (
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-medium text-foreground">Credentials</h3>
          <Skeleton className="h-9 w-32" />
        </div>
        <div className="space-y-3">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
        </div>
      </div>
    );
  }

  const githubCredentials =
    credentialsData?.credentials.filter(
      (credential) => credential.kind === "github_pat",
    ) ?? [];

  if (githubCredentials.length === 0) {
    return (
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="text-sm font-medium text-foreground">Credentials</h3>
            <p className="text-xs text-muted-foreground">
              Stored provider credentials for repository and model access.
            </p>
          </div>
          <Button onClick={handleAddCredential} size="sm" variant="outline">
            <Plus className="mr-2 h-4 w-4" />
            Add credential
          </Button>
        </div>
        <section className="space-y-3">
          <CredentialSectionHeader
            title="GitHub"
            description="Personal access tokens used for repository access."
          />
          <EmptyState
            icon={<GitBranch className="h-8 w-8" />}
            title="No GitHub credentials configured"
            description="Add a GitHub Personal Access Token to connect repositories."
          />
        </section>
        <GitHubCredentialForm open={formOpen} onOpenChange={setFormOpen} />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium text-foreground">Credentials</h3>
          <p className="text-xs text-muted-foreground">
            Stored provider credentials for repository and model access.
          </p>
        </div>
        <Button onClick={handleAddCredential} size="sm" variant="outline">
          <Plus className="mr-2 h-4 w-4" />
          Add credential
        </Button>
      </div>

      <section className="space-y-3">
        <CredentialSectionHeader
          title="GitHub"
          description="Personal access tokens used for repository access."
        />
        <div className="space-y-3">
          {githubCredentials.map((credential) => (
            <Card key={credential.alias} className="p-4">
              <div className="flex items-start gap-3">
                <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xs bg-muted text-muted-foreground">
                  <GitBranch className="h-4 w-4" />
                </div>
                <div className="min-w-0 flex-1 space-y-2">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <h4 className="truncate text-sm font-semibold text-foreground">
                        {credential.alias}
                      </h4>
                      <p className="text-xs text-muted-foreground">
                        Updated{" "}
                        {new Date(credential.updatedAt).toLocaleString()}
                      </p>
                    </div>
                    <span className="rounded-sm bg-muted px-2 py-1 font-mono text-xs text-foreground">
                      {credential.secretPreview ?? "preview unavailable"}
                    </span>
                  </div>
                </div>
              </div>
            </Card>
          ))}
        </div>
      </section>

      <GitHubCredentialForm open={formOpen} onOpenChange={setFormOpen} />
    </div>
  );
}

function CredentialSectionHeader({
  title,
  description,
}: {
  readonly title: string;
  readonly description: string;
}) {
  return (
    <div className="flex items-center gap-2">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-xs bg-muted text-muted-foreground">
        <GitBranch className="h-4 w-4" />
      </div>
      <div className="min-w-0">
        <h4 className="text-sm font-semibold text-foreground">{title}</h4>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
    </div>
  );
}
