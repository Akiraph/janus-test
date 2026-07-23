import { Check, Users } from "lucide-react";
import { DropdownMenu } from "../../../components/ui/dropdown-menu";
import { useModelGatewayStatus } from "../../settings/hooks/useModelGatewayStatus";

interface ComposerDiscussionModelPickerProps {
  readonly selectedModelIds: readonly string[];
  readonly onSelectedModelIdsChange: (modelIds: readonly string[]) => void;
}

interface DiscussionModelOption {
  readonly id: string;
  readonly label: string;
}

export function ComposerDiscussionModelPicker({
  selectedModelIds,
  onSelectedModelIdsChange,
}: ComposerDiscussionModelPickerProps) {
  const { data: status } = useModelGatewayStatus();
  const selectedIds = new Set(selectedModelIds);
  const options: DiscussionModelOption[] = (status?.providers ?? [])
    .filter(
      ({ provider }) =>
        provider.client === "supervisor" &&
        provider.enabled &&
        provider.discussionEnabled,
    )
    .flatMap(({ provider }) =>
      Object.keys(provider.models).map((alias) => ({
        id: `${provider.id}:${alias}`,
        label: `${provider.name}/${alias}`,
      })),
    );
  const selectedCount = selectedModelIds.length;
  const currentLabel =
    selectedCount === 0
      ? "Discussion auto"
      : selectedCount === 1
        ? "1 model"
        : `${selectedCount} models`;

  const toggleModel = (modelId: string) => {
    const next = selectedIds.has(modelId)
      ? selectedModelIds.filter((id) => id !== modelId)
      : [...selectedModelIds, modelId];
    onSelectedModelIdsChange(next);
  };

  return (
    <DropdownMenu
      className="max-w-[18rem]"
      trigger={
        <button
          type="button"
          aria-label={`Discussion models: ${currentLabel}`}
          title="Discussion models"
          className="flex h-8 max-w-[12rem] items-center gap-1.5 rounded-md px-2 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <Users className="h-4 w-4 shrink-0" />
          <span className="truncate text-xs font-medium">{currentLabel}</span>
        </button>
      }
      items={
        options.length === 0
          ? [
              {
                id: "empty",
                label: "No discussion models",
                disabled: true,
                onClick: () => {},
              },
            ]
          : [
              {
                id: "auto",
                label: "Auto discussion",
                icon:
                  selectedCount === 0 ? (
                    <Check className="h-4 w-4" />
                  ) : undefined,
                onClick: () => onSelectedModelIdsChange([]),
              },
              "separator",
              ...options.map((option) => ({
                id: option.id,
                label: option.label,
                icon: selectedIds.has(option.id) ? (
                  <Check className="h-4 w-4" />
                ) : undefined,
                onClick: () => toggleModel(option.id),
              })),
            ]
      }
    />
  );
}
