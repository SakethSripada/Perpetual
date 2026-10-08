import * as Dropdown from "@radix-ui/react-dropdown-menu";
import * as Context from "@radix-ui/react-context-menu";
import { Fragment, useState } from "react";
import { Icon } from "./icons";
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


/** A single themed choice control for forms and composer options. */
export function ChoiceSelect({ label, value, options, onChange, disabled = false, searchable = false }: {
  label: string; value: string; options: { value: string; label: string }[];
  onChange(value: string): void; disabled?: boolean; searchable?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const selected = options.find((option) => option.value === value);
  const matches = options.filter((option) => option.label.toLowerCase().includes(query.trim().toLowerCase()));
  return <ActionMenu label={label} open={open} onOpenChange={(next) => { setOpen(next); if (!next) setQuery(""); }} align="start" className="choice-menu" trigger={<button type="button" className="choice-select" aria-label={label} disabled={disabled}><span>{selected?.label ?? value}</span><Icon name="caret" /></button>}>
    {searchable && <div className="choice-search"><Icon name="search" /><input aria-label={`Search ${label.toLowerCase()}`} placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key !== "Escape" && event.key !== "Tab" && event.key !== "ArrowDown" && event.key !== "ArrowUp") event.stopPropagation(); }} /></div>}
    {matches.map((option) => <MenuItem role="menuitemradio" aria-checked={option.value === value} key={option.value} onSelect={() => onChange(option.value)}><span>{option.label}</span>{option.value === value && <Icon name="check" />}</MenuItem>)}
    {!matches.length && <p className="menu-empty">No matching models</p>}
  </ActionMenu>;
}
