import type { ModelGatewayClient } from "@janus/shared";
import { Check, SlidersHorizontal } from "lucide-react";
import { DropdownMenu } from "../../../components/ui/dropdown-menu";
import { useModelGatewayStatus } from "../../settings/hooks/useModelGatewayStatus";
import { useSetActiveRoute } from "../../settings/hooks/useSetActiveRoute";

interface ModelOption {
  readonly providerId: string;
  readonly client: ModelGatewayClient;
  readonly alias: string;
  readonly label: string;
}

/**
 * ComposerModelPicker — model selector that lives inside the message composer.
 *
 * Lists every configured **supervisor** provider's every alias as
 * `Provider/alias` (e.g. `Anthropic/claude-opus-4-8`); claude-code and codex
 * providers are excluded here (they are activated from Settings). Selecting one
 * sets the global supervisor active route (providerId + alias).
 */
export function ComposerModelPicker() {
  const { data: status } = useModelGatewayStatus();
  const setActiveRoute = useSetActiveRoute();

  const activeRoute = status?.activeRoutes.supervisor;
  const options: ModelOption[] = (status?.providers ?? [])
    .filter(({ provider }) => provider.client === "supervisor")
    .flatMap(({ provider }) =>
      Object.keys(provider.models).map((alias) => ({
        providerId: provider.id,
        client: provider.client,
        alias,
        label: `${provider.name}/${alias}`,
      })),
    );

  const active = options.find(
    (o) =>
      o.providerId === activeRoute?.providerId &&
      o.alias === activeRoute?.modelAlias,
  );
  const currentLabel = active?.label ?? "Select model";

  const handleSelect = (option: ModelOption) => {
    setActiveRoute.mutate({
      app: option.client,
      providerId: option.providerId,
      modelAlias: option.alias,
    });
  };

  return (
    <DropdownMenu
      align="end"
      trigger={
        <button
          type="button"
          aria-label={`Model: ${currentLabel}`}
          className="flex h-8 max-w-[16rem] items-center gap-1.5 rounded-md px-2 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <SlidersHorizontal className="h-4 w-4 shrink-0" />
          <span className="truncate text-xs font-medium">{currentLabel}</span>
        </button>
      }
      items={
        options.length === 0
          ? [
              {
                id: "empty",
                label: "No providers configured",
                disabled: true,
                onClick: () => {},
              },
            ]
          : options.map((option) => ({
              id: `${option.providerId}:${option.alias}`,
              label: option.label,
              icon:
                option === active ? <Check className="h-4 w-4" /> : undefined,
              onClick: () => handleSelect(option),
            }))
      }
    />
  );
}
