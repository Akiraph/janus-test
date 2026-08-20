import type { JSX } from "solid-js";
import "./button.css";

export type ButtonVariant = "primary" | "outline" | "ghost" | "destructive";
export type ButtonSize = "sm" | "md";

interface ButtonProps {
  variant?: ButtonVariant;
  size?: ButtonSize;
  iconOnly?: boolean;
  type?: "button" | "submit" | "reset";
  disabled?: boolean;
  class?: string;
  onClick?: JSX.EventHandler<HTMLButtonElement, MouseEvent>;
  "aria-label"?: string;
  "aria-pressed"?: boolean;
  title?: string;
  children?: JSX.Element;
}

export function Button(props: ButtonProps) {
  return (
    <button
      type={props.type ?? "button"}
      class="ui-button"
      classList={{
        "ui-button--sm": (props.size ?? "md") === "sm",
        "ui-button--icon": !!props.iconOnly,
        [props.class ?? ""]: !!props.class,
      }}
      data-variant={props.variant ?? "primary"}
      disabled={props.disabled}
      onClick={props.onClick}
      aria-label={props["aria-label"]}
      aria-pressed={props["aria-pressed"]}
      title={props.title}
    >
      {props.children}
    </button>
  );
}
