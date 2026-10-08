import * as Dropdown from "@radix-ui/react-dropdown-menu";
import type { ComponentProps, ReactNode } from "react";

/** The desktop's menu primitive: collision-aware placement and native key handling. */
export function ActionMenu(props: { trigger: ReactNode; children: ReactNode; open?: boolean; onOpenChange?(open: boolean): void; side?: "top" | "bottom"; align?: "start" | "end"; label: string; className?: string }) {
  return <Dropdown.Root open={props.open} onOpenChange={props.onOpenChange} modal={false}>
    <Dropdown.Trigger asChild>{props.trigger}</Dropdown.Trigger>
    <Dropdown.Portal>
      <Dropdown.Content className={`action-menu ${props.className ?? ""}`} aria-label={props.label} side={props.side ?? "bottom"} align={props.align ?? "end"} sideOffset={6} collisionPadding={12}>
        {props.children}
      </Dropdown.Content>
    </Dropdown.Portal>
  </Dropdown.Root>;
}
export function MenuItem({ danger, children, ...props }: ComponentProps<typeof Dropdown.Item> & { danger?: boolean }) {
  return <Dropdown.Item {...props} className={`action-menu-item${danger ? " danger-text" : ""} ${props.className ?? ""}`}>{children}</Dropdown.Item>;
}
export function MenuLabel({ children }: { children: ReactNode }) { return <Dropdown.Label className="action-menu-label">{children}</Dropdown.Label>; }
export function MenuSeparator() { return <Dropdown.Separator className="action-menu-separator" />; }

