import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../../components/ui/button";
import { Dialog } from "../../../components/ui/dialog";
import { Input } from "../../../components/ui/input";
import { Select } from "../../../components/ui/select";
import { listCredentials } from "../../../lib/api-client";
import { useCreateSession } from "../hooks";

interface NewSessionDialogProps {
  projectId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSuccess?: (sessionId: string) => void;
}

export function NewSessionDialog({
  projectId,
  open,
  onOpenChange,
  onSuccess,
}: NewSessionDialogProps) {
  const [llmCredentialAlias, setLlmCredentialAlias] = useState("");
  const [dockerImage, setDockerImage] = useState("janus-cli-worker:dev");

  const { data: credentialsData, isLoading: isLoadingCredentials } = useQuery({
    queryKey: ["credentials"],
    queryFn: listCredentials,
    enabled: open,
  });

  const createSessionMutation = useCreateSession();

  const llmCredentials =
    credentialsData?.credentials.filter((c) => c.kind === "llm_api_key") ?? [];

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    try {
      const response = await createSessionMutation.mutateAsync({
        projectId,
        ...(llmCredentialAlias ? { llmCredentialAlias } : {}),
        image: dockerImage,
      });

      // Close dialog and call success callback
      onOpenChange(false);
      onSuccess?.(response.session.id);

      // Reset form
      setLlmCredentialAlias("");
      setDockerImage("janus-cli-worker:dev");
    } catch {
      // Error is handled by TanStack Query and displayed in the UI
    }
  };

  const handleCancel = () => {
    onOpenChange(false);
    // Reset form on cancel
    setLlmCredentialAlias("");
    setDockerImage("janus-cli-worker:dev");
  };

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title="New session"
      description="Create a new conversation session for this project."
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        {/* LLM Credential (optional) */}
        <div className="space-y-2">
          <label
            htmlFor="llm-credential"
            className="text-sm font-medium leading-none"
          >
            LLM Credential{" "}
            <span className="text-muted-foreground">(optional)</span>
          </label>
          <Select
            value={llmCredentialAlias}
            onValueChange={setLlmCredentialAlias}
            placeholder={
              isLoadingCredentials
                ? "Loading credentials..."
                : llmCredentials.length === 0
                  ? "No credentials configured"
                  : "Select a credential"
            }
            disabled={isLoadingCredentials || llmCredentials.length === 0}
            options={llmCredentials.map((c) => ({
              value: c.alias,
              label: c.alias,
            }))}
          />
          {llmCredentials.length === 0 && !isLoadingCredentials && (
            <p className="text-xs text-muted-foreground">
              The session can be created now and configured later.
            </p>
          )}
        </div>

        {/* Docker Image (optional) */}
        <div className="space-y-2">
          <label
            htmlFor="docker-image"
            className="text-sm font-medium leading-none"
          >
            Docker image{" "}
            <span className="text-muted-foreground">(optional)</span>
          </label>
          <Input
            id="docker-image"
            placeholder="janus-cli-worker:dev"
            value={dockerImage}
            onChange={(e) => setDockerImage(e.target.value)}
          />
        </div>

        {/* Error message */}
        {createSessionMutation.isError && (
          <div className="rounded-md bg-destructive-soft p-3 text-sm text-destructive">
            Failed to create session. Please try again.
          </div>
        )}

        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
          <Button
            type="button"
            variant="outline"
            onClick={handleCancel}
            disabled={createSessionMutation.isPending}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={createSessionMutation.isPending}>
            {createSessionMutation.isPending ? "Creating..." : "Create session"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
