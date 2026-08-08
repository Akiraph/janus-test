import { useQueryClient } from "@tanstack/solid-query";
import Bell from "lucide-solid/icons/bell";
import ChevronDown from "lucide-solid/icons/chevron-down";
import Pencil from "lucide-solid/icons/pencil";
import Plus from "lucide-solid/icons/plus";
import Send from "lucide-solid/icons/send";
import Trash2 from "lucide-solid/icons/trash-2";
import { createSignal, For, Show } from "solid-js";
import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { EmptyState } from "../../components/ui/EmptyState";
import { NotificationEvent, useNotifications } from "../../components/ui/notifications";
import { Select, type SelectOption } from "../../components/ui/Select";
import type {
  NotificationChannelInput,
  NotificationChannelKind,
  NotificationChannelView,
  NotificationEventKind,
} from "../../lib/api";
import {
  createNotificationChannel,
  deleteNotificationChannel,
  getErrorMessage,
  testNotificationChannel,
  updateNotificationChannel,
} from "../../lib/api";
import { useNotificationChannels } from "../../lib/queries";
import "./notifications.css";

const KIND_OPTIONS: readonly SelectOption[] = [
  { value: "webhook", label: "Webhook" },
  { value: "qqbot", label: "QQBot / OneBot HTTP" },
];

const EVENT_OPTIONS: readonly { value: NotificationEventKind; label: string }[] = [
  { value: "turn_completed", label: "Turn completed" },
  { value: "turn_failed", label: "Turn failed" },
  { value: "ask_opened", label: "Model asks a question" },
  { value: "model_waiting", label: "Model needs attention" },
  { value: "job_completed", label: "Async job finished" },
];

const DEFAULT_EVENTS: NotificationEventKind[] = EVENT_OPTIONS.map((event) => event.value);

export function NotificationsSettings() {
  const channels = useNotificationChannels();
  const queryClient = useQueryClient();
  const notify = useNotifications().notify;
  const [open, setOpen] = createSignal(true);
  const [editing, setEditing] = createSignal<NotificationChannelView | null>(null);
  const [formOpen, setFormOpen] = createSignal(false);

  async function refresh() {
    await queryClient.invalidateQueries({ queryKey: ["notification-channels"] });
  }

  async function remove(id: string) {
    if (!confirm("Delete this notification channel?")) return;
    try {
      await deleteNotificationChannel(id);
      notify("Notification channel deleted", { variant: "success" });
      await refresh();
    } catch (error) {
      notify(getErrorMessage(error, "Delete failed"), { variant: "danger" });
    }
  }

  async function test(id: string) {
    try {
      await testNotificationChannel(id);
      notify("Test notification sent", { variant: "success" });
    } catch (error) {
      notify(getErrorMessage(error, "Test notification failed"), { variant: "danger" });
    }
  }

  return (
    <div class="panel notification-settings">
      <section class="settings-group">
        <button
          class="settings-group-trigger"
          type="button"
          aria-expanded={open()}
          onClick={() => setOpen(!open())}
        >
          <ChevronDown classList={{ collapsed: !open() }} size={16} />
          <div>
            <span class="settings-group-title">
              <Bell size={15} /> Notifications
            </span>
            <small>Deliver turn, question, and async-job updates to external channels.</small>
          </div>
        </button>
        <Show when={open()}>
          <div class="settings-group-body">
            <Show
              when={!channels.isPending}
              fallback={<p class="surface-note">Loading notification channels...</p>}
            >
              <Show
                when={(channels.data?.length ?? 0) > 0}
                fallback={
                  <EmptyState
                    icon={Bell}
                    title="No notification channels"
                    description="Add a Webhook or QQBot target to receive Janus updates."
                  />
                }
              >
                <div class="record-list">
                  <For each={channels.data}>
                    {(channel) => (
                      <article class="record-card notification-card">
                        <div class="notification-card__main">
                          <div class="record-copy">
                            <div class="record-title">
                              <h3>{channel.display_name}</h3>
                              <span class="record-chip">
                                {channel.kind === "qqbot" ? "QQBot" : "Webhook"}
                              </span>
                              <Show when={!channel.enabled}>
                                <span class="record-chip">Disabled</span>
                              </Show>
                            </div>
                            <p>{channel.endpoint_url}</p>
                            <small>{eventSummary(channel.events)}</small>
                          </div>
                          <div class="record-actions">
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => void test(channel.id)}
                            >
                              <Send size={14} /> Test
                            </Button>
                            <Button
                              variant="ghost"
                              size="sm"
                              iconOnly
                              aria-label={`Edit ${channel.display_name}`}
                              onClick={() => {
                                setEditing(channel);
                                setFormOpen(true);
                              }}
                            >
                              <Pencil size={16} />
                            </Button>
                            <Button
                              variant="ghost"
                              size="sm"
                              iconOnly
                              aria-label={`Delete ${channel.display_name}`}
                              onClick={() => void remove(channel.id)}
                            >
                              <Trash2 size={16} />
                            </Button>
                          </div>
                        </div>
                      </article>
                    )}
                  </For>
                </div>
              </Show>
            </Show>
            <Button
              variant="outline"
              class="add-record"
              onClick={() => {
                setEditing(null);
                setFormOpen(true);
              }}
            >
              <Plus size={16} /> Add notification channel
            </Button>
          </div>
        </Show>
      </section>

      <Show when={formOpen()}>
        <ChannelForm
          channel={editing()}
          close={() => setFormOpen(false)}
          saved={async () => {
            setFormOpen(false);
            await refresh();
          }}
        />
      </Show>
    </div>
  );
}

function eventSummary(events: readonly NotificationEventKind[]): string {
  return events
    .map((event) => EVENT_OPTIONS.find((option) => option.value === event)?.label ?? event)
    .join(", ");
}

interface ChannelFormProps {
  channel: NotificationChannelView | null;
  close: () => void;
  saved: () => Promise<void>;
}

function ChannelForm(props: ChannelFormProps) {
  const notify = useNotifications().notify;
  const editing = () => props.channel !== null;
  const [kind, setKind] = createSignal<NotificationChannelKind>(props.channel?.kind ?? "webhook");
  const [name, setName] = createSignal(props.channel?.display_name ?? "");
  const [endpoint, setEndpoint] = createSignal(props.channel?.endpoint_url ?? "");
  const [secret, setSecret] = createSignal("");
  const [userId, setUserId] = createSignal(props.channel?.target.user_id ?? "");
  const [groupId, setGroupId] = createSignal(props.channel?.target.group_id ?? "");
  const [enabled, setEnabled] = createSignal(props.channel?.enabled ?? true);
  const [events, setEvents] = createSignal<NotificationEventKind[]>(
    props.channel?.events.length ? [...props.channel.events] : [...DEFAULT_EVENTS],
  );
  const [error, setError] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);

  function toggleEvent(event: NotificationEventKind, checked: boolean) {
    setEvents((current) =>
      checked ? (current.includes(event) ? current : [...current, event]) : current.filter((item) => item !== event),
    );
  }

  function buildInput(): NotificationChannelInput {
    const input: NotificationChannelInput = {
      kind: kind(),
      display_name: name().trim(),
      endpoint_url: endpoint().trim(),
      target: {
        user_id: kind() === "qqbot" && userId().trim() ? userId().trim() : null,
        group_id: kind() === "qqbot" && groupId().trim() ? groupId().trim() : null,
      },
      events: events(),
      enabled: enabled(),
    };
    if (secret().trim()) input.secret = secret().trim();
    return input;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!name().trim() || !endpoint().trim()) {
      setError("Name and endpoint are required");
      return;
    }
    if (kind() === "qqbot" && !editing() && !secret().trim()) {
      setError("QQBot token is required");
      return;
    }
    if (events().length === 0) {
      setError("Select at least one event");
      return;
    }
    setSubmitting(true);
    try {
      const input = buildInput();
      if (props.channel) {
        await updateNotificationChannel(props.channel.id, input);
        notify("Notification channel updated", { variant: "success" });
      } else {
        await createNotificationChannel(input);
        notify("Notification channel added", { variant: "success" });
      }
      await props.saved();
    } catch (value) {
      setError(getErrorMessage(value, "Notification channel could not be saved"));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog
      title={editing() ? "Edit notification channel" : "Add notification channel"}
      description="Webhook accepts a Janus JSON event. QQBot uses a OneBot-compatible HTTP send endpoint."
      close={props.close}
    >
      <form class="dialog-form" onSubmit={submit}>
        <div class="dialog-form-grid">
          <div>
            <span class="field-label">Name</span>
            <input class="ui-input" value={name()} onInput={(event) => setName(event.currentTarget.value)} required />
          </div>
          <div>
            <span class="field-label">Channel</span>
            <Select
              value={kind()}
              options={KIND_OPTIONS}
              onChange={(value) => setKind(value as NotificationChannelKind)}
              aria-label="Channel"
            />
          </div>
          <div class="full-field">
            <span class="field-label">Endpoint URL</span>
            <input
              class="ui-input"
              type="url"
              value={endpoint()}
              onInput={(event) => setEndpoint(event.currentTarget.value)}
              placeholder={kind() === "qqbot" ? "http://127.0.0.1:3000/send_msg" : "https://example.test/janus"}
              required
            />
          </div>
          <div class="full-field">
            <span class="field-label">Token / secret</span>
            <input
              class="ui-input"
              type="password"
              value={secret()}
              onInput={(event) => setSecret(event.currentTarget.value)}
              placeholder={editing() ? "Leave blank to keep the stored secret" : "Optional for Webhook"}
              autocomplete="off"
            />
          </div>
          <Show when={kind() === "qqbot"}>
            <div>
              <span class="field-label">Private user ID</span>
              <input
                class="ui-input"
                value={userId()}
                onInput={(event) => setUserId(event.currentTarget.value)}
                placeholder="OneBot user_id"
              />
            </div>
            <div>
              <span class="field-label">Group ID</span>
              <input
                class="ui-input"
                value={groupId()}
                onInput={(event) => setGroupId(event.currentTarget.value)}
                placeholder="OneBot group_id"
              />
            </div>
          </Show>
          <label class="full-field notification-toggle">
            <input type="checkbox" checked={enabled()} onChange={(event) => setEnabled(event.currentTarget.checked)} />
            Enabled
          </label>
        </div>
        <div class="notification-events">
          <span class="field-label">Events</span>
          <div class="notification-events__grid">
            <For each={EVENT_OPTIONS}>
              {(event) => (
                <label class="notification-event-option">
                  <input
                    type="checkbox"
                    checked={events().includes(event.value)}
                    onChange={(inputEvent) => toggleEvent(event.value, inputEvent.currentTarget.checked)}
                  />
                  {event.label}
                </label>
              )}
            </For>
          </div>
        </div>
        <NotificationEvent message={error()} variant="danger" />
        <div class="dialog-footer">
          <Button variant="outline" type="button" onClick={props.close}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={submitting()}>
            {editing() ? "Save changes" : "Add channel"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
