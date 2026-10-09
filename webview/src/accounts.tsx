import { useRef, useState } from "react";
import type { AgentKind, ProviderAccount, ProviderAccountStatus, WorkbenchSnapshot } from "./types";
import { Icon, ProviderLogo } from "./icons";
import { post, request } from "./bridge";
import { ActionMenu, ChoiceSelect, MenuItem, MenuLabel, MenuSeparator } from "./controls";
import { resourceState, ResourceState } from "./loading";
import { UsageLimits } from "./usage";

export const providers: AgentKind[] = ["codex", "claude_code"];
export const providerName = (agent: AgentKind) => agent === "codex" ? "Codex" : "Claude Code";
const PLAN_NAMES: Record<string, string> = { free: "Free", plus: "Plus", pro: "Pro", prolite: "Pro Lite", max: "Max", team: "Team", business: "Business", enterprise: "Enterprise", edu: "Edu" };
export const planName = (plan?: string | null) => {
  if (!plan?.trim()) return null;
  const value = plan.trim();
  return PLAN_NAMES[value.toLowerCase().replace(/[ _-]/g, "")] ?? value.replace(/[_-]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
};
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
/** Show identities once while preserving their stored profiles and credentials. */
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
  const busyRef = useRef(false);
  const choices = uniqueAccountChoices(snapshot?.providerAccounts ?? []);
  const active = activeAccount(snapshot, agent);
  const state = resourceState(snapshot, "accounts");
  const choose = async (account: ProviderAccountStatus) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { await request({ type: account.authenticated ? "activateProviderAccount" : "signInProviderAccount", accountId: account.id }); if (account.authenticated) onPickAgent?.(account.agent); setOpen(false); }
    catch (error) { setError(String(error instanceof Error ? error.message : error)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  return <ActionMenu open={open} onOpenChange={setOpen} label="Switch account" side="top" align="start" className="account-switch-menu" trigger={
    <button className="account-switch-trigger" disabled={!snapshot?.trusted} title="Choose account"><ProviderBadge agent={agent} /><span>{active ? accountName(active) : state === "loading" ? "Loading accounts…" : state === "error" ? "Accounts unavailable" : "Choose account"}</span><Icon name="caret" /></button>
  }>
    {state !== "ready" && <ResourceState state={state} label="accounts" />}
    {state === "ready" && providers.map((provider) => <div key={provider}>
      <MenuLabel>{providerName(provider)}</MenuLabel>
      {choices.filter((account) => account.agent === provider).map((account) => <MenuItem key={account.id} role="menuitemradio" aria-checked={account.active} disabled={busy || !account.installed || !account.enabled} className={`account-option ${accountState(account)}`} onSelect={(event) => { event.preventDefault(); void choose(account); }}>
        <ProviderBadge agent={provider} /><span><strong>{accountName(account)}</strong><small>{accountStateLabel(account)}{account.plan ? ` · ${planName(account.plan)}` : ""}</small></span>{account.active && <Icon name="check" />}
      </MenuItem>)}
      {!choices.some((account) => account.agent === provider) && <MenuItem disabled={busy} onSelect={onManage}>Connect {providerName(provider)}</MenuItem>}
    </div>)}
    {error && <p className="inline-error" role="alert">{error}</p>}
    <MenuSeparator />
    <MenuItem onSelect={onManage}><Icon name="settings" />Manage accounts</MenuItem>
  </ActionMenu>;
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
  const accounts = uniqueAccountChoices(snapshot.providerAccounts);
  const state = resourceState(snapshot, "accounts");
  const run = async (action: Record<string, unknown> & { type: string }, done?: () => void) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { await request(action); done?.(); }
    catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const update = (id: string, patch: Partial<ProviderAccount>, done?: () => void) => void run({ type: "updateProviderAccount", accountId: id, patch }, done);
  const move = (index: number, delta: number) => {
    // Reorder visible identities without dropping hidden sign-in profiles from storage.
    const orderedIds = snapshot.providerAccounts.map((account) => account.id);
    const from = orderedIds.indexOf(accounts[index].id);
    const to = orderedIds.indexOf(accounts[index + delta].id);
    [orderedIds[from], orderedIds[to]] = [orderedIds[to], orderedIds[from]];
    void run({ type: "reorderProviderAccounts", orderedIds });
  };
  const add = () => {
    const id = `${agent === "codex" ? "codex" : "claude"}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
    void run({ type: "addProviderAccount", account: { id, label: label.trim(), agent, enabled: true, use_credits: false, auth_mode: agent === "codex" ? "isolated_cli" : auth } }, () => { setAdding(false); setLabel(""); });
  };
  if (state !== "ready") return <div className="desktop-accounts"><h2>Accounts</h2><ResourceState state={state} label="accounts" /></div>;
  return <div className="desktop-accounts">
    <div className="account-page-heading"><h2>Accounts</h2><button className="quiet-icon" title="Refresh accounts" aria-label="Refresh accounts" disabled={busy} onClick={() => void run({ type: "refreshReadiness" })}><Icon name="refresh" /></button></div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    <div className="account-usage">{providers.map((agent) => <UsageLimits key={agent} agent={agent} provider={snapshot.agents.find((item) => item.kind === agent)} account={activeAccount(snapshot, agent)?.email ?? undefined} loading={snapshot.detectionState === "loading" || snapshot.detectionState === "idle"} error={snapshot.detectionState === "error"} />)}</div>
    <div className="account-list" aria-busy={busy}>
      {accounts.map((account, index) => <article className={`account-card ${accountState(account)}`} key={account.id}>
        <div className="account-card-main"><ProviderBadge agent={account.agent} /><div className="account-identity"><strong title={accountName(account)}>{accountName(account)}</strong><small>{providerName(account.agent)}{account.plan ? ` · ${planName(account.plan)}` : ""}</small></div><span className={`state-badge ${accountState(account)}`}>{snapshot.authPendingAccountIds?.includes(account.id) ? "Connecting…" : accountStateLabel(account)}</span></div>
        {account.availability === "limited" && account.reset_at && <p className="account-detail">Resets {new Date(account.reset_at).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}</p>}
        <div className="account-card-actions">
          {!account.installed ? <button className="secondary-btn" onClick={() => post({ type: "openExternal", url: account.agent === "codex" ? "https://developers.openai.com/codex/cli" : "https://code.claude.com/docs/en/setup" })}>Install CLI</button> : !account.authenticated ? <button className="primary-btn" disabled={busy || snapshot.authPendingAccountIds?.includes(account.id)} onClick={() => void run({ type: "signInProviderAccount", accountId: account.id })}>{account.auth_mode === "oauth_token" ? "Generate setup token" : "Sign in"}</button> : null}
          <ActionMenu label={`Manage ${accountName(account)}`} className="account-details-menu" trigger={<button className="quiet-icon" title={`Manage ${accountName(account)}`} aria-label={`Manage ${account.label || accountName(account)}`} disabled={busy}><Icon name="more" /></button>}>
            <MenuLabel>{account.label || providerName(account.agent)}</MenuLabel>
            {account.authenticated && <MenuItem disabled={busy || !account.enabled} onSelect={() => void run({ type: "activateProviderAccount", accountId: account.id })}><Icon name="check" />Use this account</MenuItem>}
            <MenuItem disabled={busy || !account.installed || snapshot.authPendingAccountIds?.includes(account.id)} onSelect={() => void run({ type: "signInProviderAccount", accountId: account.id })}><Icon name="agent" />{account.authenticated ? "Sign in again" : "Sign in"}</MenuItem>
            <MenuItem disabled={busy || !account.installed} onSelect={() => void run({ type: "openProviderAccountCli", accountId: account.id })}><Icon name="terminal" />Open terminal</MenuItem>
            {account.auth_mode === "oauth_token" && <MenuItem disabled={busy} onSelect={() => { setTokenId(account.id); setToken(""); }}>Paste setup token</MenuItem>}
            <MenuSeparator />
            <MenuItem disabled={busy} onSelect={() => { setEditing(account.id); setRename(account.label); }}>Rename</MenuItem>
            <MenuItem disabled={busy || index === 0} onSelect={() => move(index, -1)}><Icon name="up" />Move up</MenuItem>
            <MenuItem disabled={busy || index === accounts.length - 1} onSelect={() => move(index, 1)}><Icon name="down" />Move down</MenuItem>
            <MenuItem disabled={busy} onSelect={() => update(account.id, { enabled: !account.enabled })}>{account.enabled ? "Pause rotation" : "Resume rotation"}</MenuItem>
            {account.agent === "codex" && <MenuItem disabled={busy} onSelect={() => account.use_credits ? update(account.id, { use_credits: false }) : setCredits(account)}>{account.use_credits ? "Stop using reset credits" : "Use earned reset credits"}</MenuItem>}
            <MenuSeparator />
            <MenuItem danger disabled={busy} onSelect={() => setRemove(account)}><Icon name="trash" />Remove account</MenuItem>
          </ActionMenu>
        </div>
        {account.detail && <p className="account-detail">{account.detail}</p>}
      </article>)}
      {accounts.length === 0 && <div className="accounts-empty"><Icon name="agent" /><strong>Connect your first account</strong><p>Use your existing CLI sign-in or add a separate profile.</p></div>}
    </div>
    <div className="account-add-actions"><button className="secondary-btn" disabled={busy} onClick={() => setAdding(true)}><Icon name="plus" />Add account</button>{providers.filter((agent) => !snapshot.providerAccounts.some((account) => account.agent === agent && account.auth_mode === "system")).map((agent) => <button className="text-btn" disabled={busy} key={agent} onClick={() => void run({ type: "addSystemProviderAccount", agent })}>Connect {providerName(agent)} CLI</button>)}</div>
    {adding && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (label.trim()) add(); }}><h3>Add account</h3><label>Provider<ChoiceSelect label="Account provider" value={agent} disabled={busy} onChange={(value) => { setAgent(value as AgentKind); setAuth("isolated_cli"); }} options={[{value: "codex", label: "Codex"}, {value: "claude_code", label: "Claude Code"}]} /></label><label>Account name<input autoFocus value={label} disabled={busy} onChange={(event) => setLabel(event.target.value)} placeholder="Personal or work" maxLength={128} /></label>{agent === "claude_code" && <label>Authentication<ChoiceSelect label="Account authentication" value={auth} disabled={busy} onChange={(value) => setAuth(value as typeof auth)} options={[{value: "isolated_cli", label: "Browser sign-in"}, {value: "oauth_token", label: "Setup token"}]} /></label>}<p>This profile has its own credentials and provider session history.</p><div><button className="secondary-btn" type="button" disabled={busy} onClick={() => setAdding(false)}>Cancel</button><button className="primary-btn" disabled={busy || !label.trim()}>{busy ? "Adding…" : "Add account"}</button></div></form>}
    {editing && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (rename.trim()) update(editing, { label: rename.trim() }, () => setEditing(null)); }}><h3>Rename account</h3><input aria-label="Account name" autoFocus value={rename} disabled={busy} onChange={(event) => setRename(event.target.value)} maxLength={128} /><div><button type="button" className="secondary-btn" disabled={busy} onClick={() => setEditing(null)}>Cancel</button><button className="primary-btn" disabled={busy || !rename.trim()}>Save name</button></div></form>}
    {tokenId && <form className="account-form" onSubmit={(event) => { event.preventDefault(); if (token.trim()) void run({ type: "setProviderAccountToken", accountId: tokenId, token: token.trim() }, () => { setToken(""); setTokenId(null); }); }}><h3>Connect with a setup token</h3><p>Generate a token in the provider terminal, then paste it here.</p><input aria-label="Setup token" autoFocus type="password" autoComplete="off" value={token} disabled={busy} onChange={(event) => setToken(event.target.value)} /><div><button type="button" className="secondary-btn" disabled={busy} onClick={() => { setToken(""); setTokenId(null); }}>Cancel</button><button className="primary-btn" disabled={busy || !token.trim()}>{busy ? "Connecting…" : "Store token"}</button></div></form>}
    {remove && <div className="account-form" role="alertdialog" aria-label="Remove account"><h3>Remove {accountName(remove)}?</h3><p>{remove.auth_mode === "system" ? "Your shared CLI sign-in will stay signed in." : "This removes the saved credentials and isolated profile from Perpetual."} Existing conversations stay available.</p><div><button className="secondary-btn" disabled={busy} onClick={() => setRemove(null)}>Cancel</button><button className="primary-btn danger" disabled={busy} onClick={() => void run({ type: "deleteProviderAccount", accountId: remove.id }, () => setRemove(null))}>Remove account</button></div></div>}
    {credits && <div className="account-form" role="alertdialog" aria-label="Use reset credits"><h3>Use earned reset credits?</h3><p>This allows Codex to use optional reset credits for {accountName(credits)}. It is off by default.</p><div><button className="secondary-btn" disabled={busy} onClick={() => setCredits(null)}>Cancel</button><button className="primary-btn" disabled={busy} onClick={() => update(credits.id, { use_credits: true }, () => setCredits(null))}>Enable credits</button></div></div>}
  </div>;
}
