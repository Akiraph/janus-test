import * as DropdownMenuPrimitive from "@radix-ui/react-dropdown-menu";
import { forwardRef, type ReactNode } from "react";
import { cn } from "../../lib/cn";

export interface DropdownMenuItem {
  readonly label: string;
  readonly onClick: () => void;
  readonly icon?: ReactNode;
  readonly disabled?: boolean;
  readonly id?: string;
}

export interface DropdownMenuProps {
  readonly trigger: ReactNode;
  readonly items: readonly (DropdownMenuItem | "separator")[];
  readonly className?: string;
  readonly open?: boolean;
  readonly onOpenChange?: (open: boolean) => void;
  readonly modal?: boolean;
  readonly side?: React.ComponentPropsWithoutRef<
    typeof DropdownMenuPrimitive.Content
  >["side"];
  readonly align?: React.ComponentPropsWithoutRef<
    typeof DropdownMenuPrimitive.Content
  >["align"];
}

export function DropdownMenu({
  trigger,
  items,
  className,
  open,
  onOpenChange,
  modal,
  side,
  align,
}: DropdownMenuProps) {
  return (
    <DropdownMenuPrimitive.Root
      {...(open === undefined ? {} : { open })}
      {...(onOpenChange === undefined ? {} : { onOpenChange })}
      {...(modal === undefined ? {} : { modal })}
    >
      <DropdownMenuPrimitive.Trigger asChild>
        {trigger}
      </DropdownMenuPrimitive.Trigger>

      <DropdownMenuPrimitive.Portal>
        <DropdownMenuPrimitive.Content
          className={cn(
            "z-50 min-w-[8rem] overflow-hidden rounded-md border border-border bg-card p-1 text-foreground shadow-md data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2",
            className,
          )}
          {...(side === undefined ? {} : { side })}
          {...(align === undefined ? {} : { align })}
          sideOffset={4}
          collisionPadding={8}
        >
          {items.map((item, index) => {
            if (item === "separator") {
              // Generate stable key based on surrounding items
              const prevItem = items[index - 1];
              const nextItem = items[index + 1];
              const prevLabel =
                prevItem && prevItem !== "separator" ? prevItem.label : "start";
              const nextLabel =
                nextItem && nextItem !== "separator" ? nextItem.label : "end";
              return (
                <DropdownMenuSeparator key={`sep-${prevLabel}-${nextLabel}`} />
              );
            }
            return (
              <DropdownMenuItemComponent
                key={item.id ?? item.label}
                icon={item.icon}
                onClick={item.onClick}
                {...(item.disabled !== undefined
                  ? { disabled: item.disabled }
                  : {})}
              >
                {item.label}
              </DropdownMenuItemComponent>
            );
          })}
        </DropdownMenuPrimitive.Content>
      </DropdownMenuPrimitive.Portal>
    </DropdownMenuPrimitive.Root>
  );
}

const DropdownMenuItemComponent = forwardRef<
  React.ElementRef<typeof DropdownMenuPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof DropdownMenuPrimitive.Item> & {
    icon?: ReactNode;
  }
>(({ className, children, icon, ...props }, ref) => (
  <DropdownMenuPrimitive.Item
    ref={ref}
    className={cn(
      "relative flex cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none transition-colors hover:bg-muted focus:bg-muted data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
      className,
    )}
    {...props}
  >
    {icon && (
      <span className="flex h-4 w-4 items-center justify-center">{icon}</span>
    )}
    {children}
  </DropdownMenuPrimitive.Item>
));
DropdownMenuItemComponent.displayName = "DropdownMenuItem";

const DropdownMenuSeparator = forwardRef<
  React.ElementRef<typeof DropdownMenuPrimitive.Separator>,
  React.ComponentPropsWithoutRef<typeof DropdownMenuPrimitive.Separator>
>(({ className, ...props }, ref) => (
  <DropdownMenuPrimitive.Separator
    ref={ref}
    className={cn("-mx-1 my-1 h-px bg-border", className)}
    {...props}
  />
));
DropdownMenuSeparator.displayName = "DropdownMenuSeparator";
