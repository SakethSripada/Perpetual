import * as Dropdown from "@radix-ui/react-dropdown-menu";
import * as Context from "@radix-ui/react-context-menu";
import { Fragment } from "react";
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

export type RowAction = { label: string; icon?: ReactNode; disabled?: boolean; danger?: boolean; separated?: boolean; onSelect(): void };
export function RowActions({ actions, context = false }: { actions: RowAction[]; context?: boolean }) {
  const Item = context ? Context.Item : Dropdown.Item;
  const Separator = context ? Context.Separator : Dropdown.Separator;
  return <>{actions.map((action) => <Fragment key={action.label}>{action.separated && <Separator className="action-menu-separator" />}<Item disabled={action.disabled} onSelect={action.onSelect} className={`action-menu-item${action.danger ? " danger-text" : ""}`}>{action.icon}{action.label}</Item></Fragment>)}</>;
}
export function ContextActions({ actions, children }: { actions: RowAction[]; children: ReactNode }) {
  return <Context.Root modal={false}><Context.Trigger asChild>{children}</Context.Trigger><Context.Portal><Context.Content className="action-menu" collisionPadding={12}><RowActions actions={actions} context /></Context.Content></Context.Portal></Context.Root>;
}

