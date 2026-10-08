import type { AgentThread, WorkbenchSnapshot } from "./types";
import { BrandMark, Icon } from "./icons";
import { AccountSwitcher } from "./accounts";
import { SessionCollection } from "./sessions";
export { statusLabel } from "./sessions";

export function SessionSidebar(props: { snapshot: WorkbenchSnapshot | null; selected: AgentThread | null; agent: "codex" | "claude_code"; onNew(): void; onSelect(id: string): void; onDelete(id: string, force: boolean): void | Promise<void>; onAccounts(): void; onAgent(agent: "codex" | "claude_code"): void; onSettings(): void; onReview(id: string): void }) {
  return <aside className="session-sidebar" aria-label="Sessions"><div className="sidebar-brand"><BrandMark size={22} /><strong>Perpetual</strong><button className="icon-btn" title="Settings" aria-label="Settings" onClick={props.onSettings}><Icon name="settings" /></button></div><button className="new-session-button" onClick={props.onNew}><Icon name="plus" />New conversation</button><SessionCollection {...props} selectedId={props.selected?.id} /><div className="sidebar-footer"><AccountSwitcher snapshot={props.snapshot} agent={props.agent} onManage={props.onAccounts} onPickAgent={props.onAgent} /></div></aside>;
}
