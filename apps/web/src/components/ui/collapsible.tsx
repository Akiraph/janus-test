import * as CollapsiblePrimitive from "@radix-ui/react-collapsible";
import type { ComponentPropsWithoutRef, FunctionComponent } from "react";

type RootProps = ComponentPropsWithoutRef<typeof CollapsiblePrimitive.Root>;
type TriggerProps = ComponentPropsWithoutRef<
  typeof CollapsiblePrimitive.Trigger
>;
type ContentProps = ComponentPropsWithoutRef<
  typeof CollapsiblePrimitive.Content
>;

export const Collapsible: FunctionComponent<RootProps> = (props) => (
  <CollapsiblePrimitive.Root {...props} />
);

export const CollapsibleTrigger: FunctionComponent<TriggerProps> = (props) => (
  <CollapsiblePrimitive.Trigger {...props} />
);

export const CollapsibleContent: FunctionComponent<ContentProps> = (props) => (
  <CollapsiblePrimitive.Content {...props} />
);
