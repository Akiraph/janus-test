import {
  type ModelConfig,
  type ModelGatewayClient,
  type ModelProviderRecord,
  modelConfigModelId,
  modelConfigReasoningEffort,
} from "@janus/shared";
import { ChevronDown, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "../../components/ui/collapsible";
import { Dialog, DialogFooter } from "../../components/ui/dialog";
import { EmptyState } from "../../components/ui/empty-state";
import { Skeleton } from "../../components/ui/skeleton";
import { cn } from "../../lib/cn";
import { useDeleteProvider } from "./hooks/useDeleteProvider";
import { useModelGatewayStatus } from "./hooks/useModelGatewayStatus";
import { useModelProviders } from "./hooks/useModelProviders";
import { useSetActiveRoute } from "./hooks/useSetActiveRoute";
import { ProviderForm } from "./ProviderForm";
import {
  CLIENT_DESCRIPTIONS,
  CLIENT_LABELS,
  CLIENT_ORDER,
} from "./provider-clients";

/**
 * ProvidersPanel — model providers grouped by client.
 *
 * Each client (Supervisor / Claude Code / Codex) is its own collapsible
 * section; all three are visible at once and expand independently.
 */
export function ProvidersPanel() {
  const { data: providersData, isLoading } = useModelProviders();
  const { data: statusData } = useModelGatewayStatus();
  const setActiveRouteMutation = useSetActiveRoute();
  const deleteProviderMutation = useDeleteProvider();

  // All sections start expanded.
  const [openClients, setOpenClients] = useState<Set<ModelGatewayClient>>(
    () => new Set(CLIENT_ORDER),
  );
  const [formClient, setFormClient] =
    useState<ModelGatewayClient>("supervisor");
  const [formOpen, setFormOpen] = useState(false);
  const [formResetKey, setFormResetKey] = useState(0);
  const [editingProvider, setEditingProvider] =
    useState<ModelProviderRecord | null>(null);
  const [confirmDeleteProviderId, setConfirmDeleteProviderId] = useState<
    string | null
  >(null);

  const toggleClient = (client: ModelGatewayClient) => {
    setOpenClients((prev) => {
      const next = new Set(prev);
      if (next.has(client)) {
        next.delete(client);
      } else {
        next.add(client);
      }
      return next;
    });
  };

  const handleAdd = (client: ModelGatewayClient) => {
    setEditingProvider(null);
    setFormClient(client);
    setFormResetKey((key) => key + 1);
    setFormOpen(true);
  };

  const handleEdit = (provider: ModelProviderRecord) => {
    setEditingProvider(provider);
    setFormClient(provider.client ?? "claude-code");
    setFormResetKey((key) => key + 1);
    setFormOpen(true);
  };

  const handleActivate = async (
    client: ModelGatewayClient,
    providerId: string,
  ) => {
    try {
      await setActiveRouteMutation.mutateAsync({ app: client, providerId });
    } catch {
      // Error handled by TanStack Query.
    }
  };

  const handleDelete = async (providerId: string) => {
    try {
      await deleteProviderMutation.mutateAsync(providerId);
      setConfirmDeleteProviderId(null);
    } catch {
      // Error handled by TanStack Query.
    }
  };

  const allProviders = providersData?.providers ?? [];
  const activeRoutes = statusData?.activeRoutes ?? {};
  const confirmDeleteProvider = allProviders.find(
    (provider) => provider.id === confirmDeleteProviderId,
  );

  return (
    <div className="space-y-3">
      {CLIENT_ORDER.map((client) => {
        const providers = allProviders.filter(
          (p) => (p.client ?? "claude-code") === client,
        );
        const isOpen = openClients.has(client);
        const activeRoute = activeRoutes[client];

        return (
          <Collapsible
            key={client}
            open={isOpen}
            onOpenChange={() => toggleClient(client)}
            className="rounded-md border border-border bg-background"
          >
            <CollapsibleTrigger className="flex w-full items-center gap-3 px-4 py-3 text-left">
              <ChevronDown
                className={cn(
                  "h-4 w-4 shrink-0 text-muted-foreground transition-transform",
                  isOpen ? "" : "-rotate-90",
                )}
              />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-semibold text-foreground">
                    {CLIENT_LABELS[client]}
                  </span>
                  <Badge tone="neutral">{providers.length}</Badge>
                </div>
                <p className="truncate text-xs text-muted-foreground">
                  {CLIENT_DESCRIPTIONS[client]}
                </p>
              </div>
            </CollapsibleTrigger>

            <CollapsibleContent>
              <div className="space-y-3 border-t border-border p-4">
                {isLoading ? (
                  <Skeleton className="h-24 w-full" />
                ) : providers.length === 0 ? (
                  <EmptyState
                    icon={<Plus className="h-7 w-7" />}
                    title={`No ${CLIENT_LABELS[client]} providers`}
                    description="Add a provider to enable API access for this client."
                  />
                ) : (
                  providers.map((provider) => (
                    <ProviderCard
                      key={provider.id}
                      provider={provider}
                      isActive={
                        activeRoute?.app === client &&
                        activeRoute.providerId === provider.id
                      }
                      activating={setActiveRouteMutation.isPending}
                      showActivate={client !== "supervisor"}
                      onActivate={() => handleActivate(client, provider.id)}
                      onEdit={() => handleEdit(provider)}
                      onRequestDelete={() =>
                        setConfirmDeleteProviderId(provider.id)
                      }
                      deleting={deleteProviderMutation.isPending}
                    />
                  ))
                )}

                <Button
                  onClick={() => handleAdd(client)}
                  size="sm"
                  variant="outline"
                  className="w-full"
                >
                  <Plus className="mr-2 h-4 w-4" />
                  Add {CLIENT_LABELS[client]} provider
                </Button>
              </div>
            </CollapsibleContent>
          </Collapsible>
        );
      })}

      <ProviderForm
        key={formResetKey}
        open={formOpen}
        onOpenChange={setFormOpen}
        provider={editingProvider}
        client={formClient}
      />

      <Dialog
        open={confirmDeleteProvider !== undefined}
        onOpenChange={(open) => {
          if (!open) {
            setConfirmDeleteProviderId(null);
          }
        }}
        title="Delete provider?"
        description="This removes its model settings, health state, active route, and stored API key."
        className="max-w-md"
      >
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {confirmDeleteProvider === undefined
              ? "This provider will be deleted."
              : `Delete ${confirmDeleteProvider.name}?`}
          </p>
          {deleteProviderMutation.isError && (
            <p className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
              Failed to delete provider. Please try again.
            </p>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setConfirmDeleteProviderId(null)}
              disabled={deleteProviderMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                if (confirmDeleteProvider !== undefined) {
                  void handleDelete(confirmDeleteProvider.id);
                }
              }}
              disabled={deleteProviderMutation.isPending}
            >
              {deleteProviderMutation.isPending ? "Deleting..." : "Delete"}
            </Button>
          </DialogFooter>
        </div>
      </Dialog>
    </div>
  );
}

interface ProviderCardProps {
  readonly provider: ModelProviderRecord;
  readonly isActive: boolean;
  readonly activating: boolean;
  readonly showActivate: boolean;
  readonly onActivate: () => void;
  readonly onEdit: () => void;
  readonly onRequestDelete: () => void;
  readonly deleting: boolean;
}

function ProviderCard({
  provider,
  isActive,
  activating,
  showActivate,
  onActivate,
  onEdit,
  onRequestDelete,
  deleting,
}: ProviderCardProps) {
  const wireApiLabel = provider.wireApi
    ? provider.wireApi === "chat"
      ? "OpenAI Chat"
      : "OpenAI Responses"
    : "Anthropic Messages";
  const modelEntries = Object.entries(provider.models);

  return (
    <Card className="p-4">
      <div className="flex items-start justify-between">
        <div className="flex-1 space-y-2">
          <div className="flex items-center gap-2">
            <h4 className="text-sm font-semibold text-foreground">
              {provider.name}
            </h4>
            {isActive && <Badge tone="success">Active</Badge>}
            {!provider.enabled && <Badge tone="neutral">Disabled</Badge>}
            {!provider.hasApiKey && <Badge tone="warning">No API key</Badge>}
            {provider.client === "supervisor" && provider.discussionEnabled ? (
              <Badge tone="info">Discussion</Badge>
            ) : null}
          </div>

          <div className="flex items-center gap-3 text-xs text-muted-foreground">
            <Badge tone="neutral">{wireApiLabel}</Badge>
          </div>

          <p className="truncate text-xs text-muted-foreground">
            {provider.upstreamBaseUrl}
          </p>

          {modelEntries.length > 0 && (
            <div className="flex flex-wrap gap-1.5 pt-1">
              {modelEntries.map(([alias, model]) => (
                <ModelMapChip
                  key={`${provider.id}:${alias}`}
                  alias={alias}
                  model={model}
                />
              ))}
            </div>
          )}
        </div>

        <div className="flex gap-2">
          {showActivate && !isActive && provider.enabled && (
            <Button
              onClick={onActivate}
              size="sm"
              variant="outline"
              disabled={activating}
            >
              Activate
            </Button>
          )}
          <Button onClick={onEdit} size="sm" variant="ghost">
            Edit
          </Button>
          <Button
            onClick={onRequestDelete}
            size="icon-sm"
            variant="ghost"
            aria-label={`Delete ${provider.name}`}
            disabled={deleting}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </Card>
  );
}

function ModelMapChip({
  alias,
  model,
}: {
  readonly alias: string;
  readonly model: ModelConfig;
}) {
  const modelId = modelConfigModelId(model);
  const reasoningEffort = modelConfigReasoningEffort(model);

  return (
    <span className="inline-flex items-center gap-1 rounded-sm bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
      <span className="font-semibold text-foreground/70">{alias}</span>
      <span aria-hidden>→</span>
      <span className="truncate">{modelId}</span>
      {reasoningEffort !== undefined && reasoningEffort !== "none" ? (
        <span className="text-foreground/70">· {reasoningEffort}</span>
      ) : null}
    </span>
  );
}
