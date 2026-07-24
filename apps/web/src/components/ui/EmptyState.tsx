import type { Component, JSX } from "solid-js";
import { Show } from "solid-js";

interface EmptyStateProps {
  icon: Component<{ size?: number; strokeWidth?: number }>;
  title: string;
  description?: string;
  action?: JSX.Element;
  class?: string;
}

export function EmptyState(props: EmptyStateProps) {
  return (
    <div class="ui-empty" classList={{ [props.class ?? ""]: !!props.class }}>
      <props.icon size={48} strokeWidth={1.6} />
      <h2>{props.title}</h2>
      <Show when={props.description}>
        <p>{props.description}</p>
      </Show>
      {props.action}
    </div>
  );
}
