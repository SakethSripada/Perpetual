import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { AgentKind, ProviderAccount, ProviderAccountStatus, WorkbenchSnapshot } from "./types";
import { Icon, ProviderLogo } from "./icons";
import { post, request } from "./bridge";

export const providers: AgentKind[] = ["codex", "claude_code"];
export const providerName = (agent: AgentKind) => agent === "codex" ? "Codex" : "Claude Code";
export const accountName = (account: ProviderAccountStatus) => account.email || account.label || `${providerName(account.agent)} sign-in`;
export function accountState(account: ProviderAccountStatus) {
  if (!account.installed) return "missing";
  if (!account.enabled) return "paused";
  if (!account.authenticated) return "signed-out";
  if (account.availability === "limited") return "limited";
  return account.active ? "active" : "ready";
}
export function accountStateLabel(account: ProviderAccountStatus) {
  const state = accountState(account);
  return ({ missing: "CLI not installed", paused: "Paused", "signed-out": "Signed out", limited: "At usage limit", active: "Active", ready: "Ready" })[state];
}
export function activeAccount(snapshot: WorkbenchSnapshot | null, agent: AgentKind) {
  return snapshot?.providerAccounts.find((account) => account.agent === agent && account.active);
}
/** Profiles remain individually manageable; the run picker lists identities. */
export function uniqueAccountChoices(accounts: ProviderAccountStatus[]) {
  const choices = new Map<string, ProviderAccountStatus>();
  for (const account of accounts) {
    const key = account.authenticated && account.email?.trim()
      ? `${account.agent}:${account.email.trim().toLowerCase()}` : `${account.agent}:profile:${account.id}`;
    const previous = choices.get(key);
    const rank = (item: ProviderAccountStatus) => item.active ? 4 : !item.enabled ? 0 : !item.installed || !item.authenticated ? 1 : item.availability === "limited" ? 2 : 3;
    if (!previous || rank(account) > rank(previous)) choices.set(key, account);
  }
  return [...choices.values()];
}
export function ProviderBadge({ agent }: { agent: AgentKind }) {
  return <span className={`provider-avatar ${agent}`} aria-hidden="true"><ProviderLogo agent={agent} /></span>;
}

export function AccountSwitcher({ snapshot, agent, onManage, onPickAgent }: { snapshot: WorkbenchSnapshot | null; agent: AgentKind; onManage(): void; onPickAgent?(agent: AgentKind): void }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number; width: number; maxHeight: number } | null>(null);
  const choices = uniqueAccountChoices(snapshot?.providerAccounts ?? []);
  const active = activeAccount(snapshot, agent);
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const anchor = root.current?.getBoundingClientRect();
      if (!anchor) return;
      const width = Math.min(320, window.innerWidth - 24);
      const maxHeight = Math.max(100, Math.min(500, anchor.top - 20));
      const height = Math.min(menu.current?.scrollHeight ?? maxHeight, maxHeight);
      setPosition({ left: Math.max(12, Math.min(anchor.left, window.innerWidth - width - 12)), top: Math.max(12, anchor.top - height - 8), width, maxHeight });
    };
    place();
    const observer = new ResizeObserver(place);
    if (menu.current) observer.observe(menu.current);
    if (root.current) observer.observe(root.current);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => { observer.disconnect(); window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true); };
  }, [open, choices.length]);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: MouseEvent) => { if (!root.current?.contains(event.target as Node) && !menu.current?.contains(event.target as Node)) setOpen(false); };
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") { setOpen(false); root.current?.querySelector<HTMLButtonElement>(".account-switch-trigger")?.focus(); }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      const items = [...(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
      if (!items.length) return;
      event.preventDefault();
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (current + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
      items[next].focus();
    };
    document.addEventListener("mousedown", dismiss); document.addEventListener("keydown", key);
    menu.current?.querySelector<HTMLButtonElement>("button[aria-checked=true]:not(:disabled), button:not(:disabled)")?.focus();
    return () => { document.removeEventListener("mousedown", dismiss); document.removeEventListener("keydown", key); };
  }, [open]);
  const choose = async (account: ProviderAccountStatus) => {
    if (busy) return;
    setBusy(true); setError(null);
    try { await request({ type: account.authenticated ? "activateProviderAccount" : "signInProviderAccount", accountId: account.id }); if (account.authenticated) onPickAgent?.(account.agent); setOpen(false); }
    catch (error) { setError(String(error instanceof Error ? error.message : error)); }
    finally { setBusy(false); }
  };
  return <div className="account-switcher" ref={root}>
    <button className="account-switch-trigger" aria-expanded={open} aria-haspopup="menu" onClick={() => setOpen(!open)} disabled={!snapshot?.trusted} title="Choose the account for your next run">
      <ProviderBadge agent={agent} /><span>{active ? accountName(active) : "Choose account"}</span><Icon name="caret" />
    </button>
    {open && createPortal(<div ref={menu} className="account-switch-menu" role="menu" aria-label="Switch account" style={{ ...position, position: "fixed", visibility: position ? "visible" : "hidden" }}>
      {providers.map((provider) => <div key={provider}>
        <div className="account-menu-label">{providerName(provider)}</div>
        {choices.filter((account) => account.agent === provider).map((account) => <button key={account.id} role="menuitemradio" aria-checked={account.active} disabled={busy || !account.installed || !account.enabled} className={`account-option ${accountState(account)}`} onClick={() => void choose(account)}>
          <ProviderBadge agent={provider} /><span><strong>{accountName(account)}</strong><small>{accountStateLabel(account)}{account.plan ? ` · ${account.plan}` : ""}</small></span>{account.active && <span aria-hidden="true">✓</span>}
        </button>)}
        {!snapshot?.providerAccounts.some((account) => account.agent === provider) && <button role="menuitem" disabled={busy} onClick={() => { setOpen(false); onManage(); }}>Connect {providerName(provider)}</button>}
      </div>)}
      {error && <p className="inline-error" role="alert">{error}</p>}
      <button role="menuitem" className="manage-accounts" onClick={() => { setOpen(false); onManage(); }}><Icon name="settings" /> Manage accounts</button>
    </div>, document.body)}
  </div>;
}

export function Accounts({ snapshot }: { snapshot: WorkbenchSnapshot }) {
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [agent, setAgent] = useState<AgentKind>("codex");
  const [label, setLabel] = useState("");
  const [auth, setAuth] = useState<"isolated_cli" | "oauth_token">("isolated_cli");
  const [editing, setEditing] = useState<string | null>(null);
  const [rename, setRename] = useState("");
  const [tokenId, setTokenId] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [remove, setRemove] = useState<ProviderAccountStatus | null>(null);
  const [credits, setCredits] = useState<ProviderAccountStatus | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const accounts = snapshot.providerAccounts;
  const run = async (action: Record<string, unknown> & { type: string }, done?: () => void) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { await request(action); done?.(); }
    catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const update = (id: string, patch: Partial<ProviderAccount>, done?: () => void) => void run({ type: "updateProviderAccount", accountId: id, patch }, done);
  const move = (index: number, delta: number) => {
    const orderedIds = accounts.map((account) => account.id);
    [orderedIds[index], orderedIds[index + delta]] = [orderedIds[index + delta], orderedIds[index]];
    void run({ type: "reorderProviderAccounts", orderedIds });
  };
  const add = () => {
    const id = `${agent === "codex" ? "codex" : "claude"}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
    void run({ type: "addProviderAccount", account: { id, label: label.trim(), agent, enabled: true, use_credits: false, auth_mode: agent === "codex" ? "isolated_cli" : auth } }, () => { setAdding(false); setLabel(""); setExpanded(id); });
  };
  return <div className="desktop-accounts">
    <div className="account-page-heading"><div><h2>Accounts</h2><p>Choose an account. Keep working when limits change.</p></div><button className="secondary-btn" disabled={busy} onClick={() => void run({ type: "refreshReadiness" })}><Icon name="refresh" />Refresh</button></div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    <div className="provider-overview">{providers.map((agent) => {
      const provider = snapshot.agents.find((item) => item.kind === agent);
      const ready = accounts.filter((account) => account.agent === agent && ["active", "ready"].includes(accountState(account))).length;
      return <div className="provider-overview-card" key={agent}><div><ProviderBadge agent={agent} /><strong>{providerName(agent)}</strong><small>{ready} ready</small></div>{(["five_hour", "weekly"] as const).map((key) => {
        const window = provider?.usage?.[key];
        return window && <div className="provider-usage" key={key}><label>{key === "five_hour" ? "5-hour" : "Weekly"}<span>{Math.round(Math.max(0, 100 - window.used_percent))}% left</span></label><progress aria-label={`${providerName(agent)} ${key === "five_hour" ? "5-hour" : "weekly"} usage`} value={Math.max(0, Math.min(100, window.used_percent))} max={100} />{window.reset_at && <small>Resets {new Date(window.reset_at).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" })}</small>}</div>;
      })}{!provider?.usage && <p>Usage appears after the next run.</p>}</div>;
    })}</div>
    <div className="account-list" aria-busy={busy}>
      {accounts.map((account, index) => <article className={`account-card ${accountState(account)}`} key={account.id}>
        <div className="account-card-main"><ProviderBadge agent={account.agent} /><div className="account-identity"><strong title={accountName(account)}>{accountName(account)}</strong><small>{providerName(account.agent)}{account.plan ? ` · ${account.plan}` : ""}{account.auth_mode === "system" ? " · Shared CLI sign-in" : account.email ? ` · ${account.label}` : " · Isolated profile"}</small></div><span className={`state-badge ${accountState(account)}`}>{snapshot.authPendingAccountIds?.includes(account.id) ? "Connecting…" : accountStateLabel(account)}</span></div>
        {account.availability === "limited" && account.reset_at && <p className="account-detail">Resets {new Date(account.reset_at).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}</p>}
        <div className="account-card-actions">
          {!account.installed ? <button className="secondary-btn" onClick={() => post({ type: "openExternal", url: account.agent === "codex" ? "https://developers.openai.com/codex/cli" : "https://code.claude.com/docs/en/setup" })}>Install CLI</button> : !account.authenticated ? <button className="primary-btn" disabled={busy || snapshot.authPendingAccountIds?.includes(account.id)} onClick={() => void run({ type: "signInProviderAccount", accountId: account.id })}>{account.auth_mode === "oauth_token" ? "Generate setup token" : "Sign in"}</button> : !account.active && <button className="secondary-btn" disabled={busy || !account.enabled} onClick={() => void run({ type: "activateProviderAccount", accountId: account.id })}>Use this account</button>}
          {account.auth_mode === "oauth_token" && <button className="secondary-btn" disabled={busy} onClick={() => { setTokenId(account.id); setToken(""); }}>Paste setup token</button>}
          <button className="account-more" aria-expanded={expanded === account.id} disabled={busy} onClick={() => setExpanded(expanded === account.id ? null : account.id)}>Manage <Icon name="caret" /></button>
        </div>
        {expanded === account.id && <div className="account-details">
          {account.detail && <p>{account.detail}</p>}
          <div className="account-detail-actions">
            <button disabled={busy || !account.installed} onClick={() => void run({ type: "openProviderAccountCli", accountId: account.id })}><Icon name="terminal" />Open terminal</button>
            <button disabled={busy || !account.installed} onClick={() => void run({ type: "signInProviderAccount", accountId: account.id })}>Sign in again</button>
            <button disabled={busy} onClick={() => { setEditing(account.id); setRename(account.label); }}>Rename</button>
            <button disabled={busy || index === 0} onClick={() => move(index, -1)}><Icon name="up" />Move up</button>
            <button disabled={busy || index === accounts.length - 1} onClick={() => move(index, 1)}><Icon name="down" />Move down</button>
            <button disabled={busy} onClick={() => update(account.id, { enabled: !account.enabled })}>{account.enabled ? "Pause rotation" : "Resume rotation"}</button>
            {account.agent === "codex" && <button disabled={busy} onClick={() => account.use_credits ? update(account.id, { use_credits: false }) : setCredits(account)}>{account.use_credits ? "Stop using reset credits" : "Use earned reset credits"}</button>}
            <button className="danger-text" disabled={busy} onClick={() => setRemove(account)}>Remove account</button>
          </div>
        </div>}
      </article>)}
      {accounts.length === 0 && <div className="accounts-empty"><Icon name="agent" /><strong>Connect your first account</strong><p>Use your existing CLI sign-in or add a separate profile.</p></div>}
    </div>
    <div className="account-add-actions"><button className="secondary-btn" disabled={busy} onClick={() => setAdding(true)}><Icon name="plus" />Add account</button>{providers.filter((agent) => !accounts.some((account) => account.agent === agent && account.auth_mode === "system")).map((agent) => <button className="text-btn" disabled={busy} key={agent} onClick={() => void run({ type: "addSystemProviderAccount", agent })}>Connect {providerName(agent)} CLI</button>)}</div>
    <p className="account-order-note">Accounts rotate in this order. Switching applies to the next run; active turns keep their account.</p>
    {adding && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (label.trim()) add(); }}><h3>Add account</h3><label>Provider<select value={agent} disabled={busy} onChange={(event) => { setAgent(event.target.value as AgentKind); setAuth("isolated_cli"); }}><option value="codex">Codex</option><option value="claude_code">Claude Code</option></select></label><label>Account name<input autoFocus value={label} disabled={busy} onChange={(event) => setLabel(event.target.value)} placeholder="Personal or work" maxLength={128} /></label>{agent === "claude_code" && <label>Authentication<select value={auth} disabled={busy} onChange={(event) => setAuth(event.target.value as typeof auth)}><option value="isolated_cli">Browser sign-in</option><option value="oauth_token">Setup token</option></select></label>}<p>This profile has its own credentials and provider session history.</p><div><button className="secondary-btn" type="button" disabled={busy} onClick={() => setAdding(false)}>Cancel</button><button className="primary-btn" disabled={busy || !label.trim()}>{busy ? "Adding…" : "Add account"}</button></div></form>}
    {editing && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (rename.trim()) update(editing, { label: rename.trim() }, () => setEditing(null)); }}><h3>Rename account</h3><input aria-label="Account name" autoFocus value={rename} disabled={busy} onChange={(event) => setRename(event.target.value)} maxLength={128} /><div><button type="button" className="secondary-btn" disabled={busy} onClick={() => setEditing(null)}>Cancel</button><button className="primary-btn" disabled={busy || !rename.trim()}>Save name</button></div></form>}
    {tokenId && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (token.trim()) void run({ type: "setProviderAccountToken", accountId: tokenId, token: token.trim() }, () => { setToken(""); setTokenId(null); }); }}><h3>Connect with a setup token</h3><p>Generate a token in the provider terminal, then paste it here.</p><input aria-label="Setup token" autoFocus type="password" autoComplete="off" value={token} disabled={busy} onChange={(event) => setToken(event.target.value)} /><div><button type="button" className="secondary-btn" disabled={busy} onClick={() => { setToken(""); setTokenId(null); }}>Cancel</button><button className="primary-btn" disabled={busy || !token.trim()}>{busy ? "Connecting…" : "Store token"}</button></div></form>}
    {remove && <div className="account-form" role="alertdialog" aria-label="Remove account"><h3>Remove {accountName(remove)}?</h3><p>{remove.auth_mode === "system" ? "Your shared CLI sign-in will stay signed in." : "This removes the saved credentials and isolated profile from Perpetual."} Existing conversations stay available.</p><div><button className="secondary-btn" disabled={busy} onClick={() => setRemove(null)}>Cancel</button><button className="primary-btn danger" disabled={busy} onClick={() => void run({ type: "deleteProviderAccount", accountId: remove.id }, () => setRemove(null))}>Remove account</button></div></div>}
    {credits && <div className="account-form" role="alertdialog" aria-label="Use reset credits"><h3>Use earned reset credits?</h3><p>This allows Codex to use optional reset credits for {accountName(credits)}. It is off by default.</p><div><button className="secondary-btn" disabled={busy} onClick={() => setCredits(null)}>Cancel</button><button className="primary-btn" disabled={busy} onClick={() => update(credits.id, { use_credits: true }, () => setCredits(null))}>Enable credits</button></div></div>}
  </div>;
}
