import ArrowUp from "lucide-solid/icons/arrow-up";
import X from "lucide-solid/icons/x";
import { createSignal, For, Show } from "solid-js";
import { useNotifications } from "../../components/ui/notifications";
import type { QueuedTurnItem } from "../../lib/api";
import { getErrorMessage } from "../../lib/api";

interface QueuedMessagesBarProps {
  turns: readonly QueuedTurnItem[];
  onDelete: (turn: QueuedTurnItem) => Promise<void>;
  onSteer?: (turn: QueuedTurnItem) => Promise<void>;
}

/**
 * Displays queued user messages that are waiting for their turn to execute.
 * Each row shows the message text and a delete button while automatic FIFO
 * promotion remains responsible for delivery order.
 */
export function QueuedMessagesBar(props: QueuedMessagesBarProps) {
  const { notify } = useNotifications();
  const [deletingId, setDeletingId] = createSignal<string | null>(null);
  const [steeringId, setSteeringId] = createSignal<string | null>(null);

  async function handleDelete(turn: QueuedTurnItem) {
    if (deletingId()) return;
    setDeletingId(turn.turn_id);
    try {
      await props.onDelete(turn);
    } catch (cause) {
      notify(getErrorMessage(cause, "Queued message could not be removed"), {
        variant: "danger",
      });
    } finally {
      setDeletingId(null);
    }
  }

  async function handleSteer(turn: QueuedTurnItem) {
    if (!props.onSteer || deletingId() || steeringId()) return;
    setSteeringId(turn.turn_id);
    try {
      await props.onSteer(turn);
    } catch (cause) {
      notify(getErrorMessage(cause, "Queued message could not steer the active turn"), {
        variant: "danger",
      });
    } finally {
      setSteeringId(null);
    }
  }

  return (
    <div class="queued-bar" role="status" aria-label="Queued messages">
      <Show when={props.turns.length > 0}>
        <span class="queued-bar__label">
          {props.turns.length === 1 ? "1 queued message" : `${props.turns.length} queued messages`}
        </span>
        <For each={props.turns}>
          {(turn) => (
            <div class="queued-bar__row">
              <span class="session-message__dot" data-tone="muted" aria-hidden="true" />
              <span class="queued-bar__text" title={turn.message_text}>
                {turn.message_text || "(empty)"}
              </span>
              <Show when={props.onSteer}>
                <button
                  type="button"
                  class="queued-bar__steer"
                  title="Steer active turn with this message"
                  aria-label="Steer active turn with this message"
                  disabled={Boolean(deletingId() || steeringId())}
                  onClick={() => void handleSteer(turn)}
                >
                  <Show when={steeringId() === turn.turn_id} fallback={<ArrowUp size={12} />}>
                    <ArrowUp size={12} class="ui-spinner" />
                  </Show>
                </button>
              </Show>
              <button
                type="button"
                class="queued-bar__delete"
                title="Remove queued message"
                aria-label="Remove queued message"
                disabled={Boolean(deletingId() || steeringId())}
                onClick={() => void handleDelete(turn)}
              >
                <X size={12} aria-hidden="true" />
              </button>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
