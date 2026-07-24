import type { JSX } from "solid-js";

export type BadgeVariant = "neutral" | "success" | "warning" | "danger";

interface BadgeProps {
  variant?: BadgeVariant;
  class?: string;
  children?: JSX.Element;
}

export function Badge(props: BadgeProps) {
  return (
    <span
      class="ui-badge"
      classList={{ [props.class ?? ""]: !!props.class }}
      data-variant={props.variant ?? "neutral"}
    >
      {props.children}
    </span>
  );
}
