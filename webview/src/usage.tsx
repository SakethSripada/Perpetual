import { memo, useMemo } from "react";
import type { AgentStatus } from "./types";
import { ProviderLogo } from "./icons";
import { Spinner } from "./loading";
import { usageWindows } from "./usageData";

const resetTime = new Intl.DateTimeFormat(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" });

export const UsageLimits = memo(function UsageLimits({ provider, agent, account, loading = false, error = false }: {
  provider?: AgentStatus; agent: "codex" | "claude_code"; account?: string; loading?: boolean; error?: boolean;
}) {
  const windows = useMemo(() => usageWindows(provider?.usage), [provider?.usage]);
  return <section className="usage-panel" aria-label={`${agent === "codex" ? "Codex" : "Claude Code"} usage limits`}>
    <div className="usage-heading"><ProviderLogo agent={agent} /><strong>{agent === "codex" ? "Codex" : "Claude Code"}</strong><span>Usage limits</span></div>
    {account && <p className="usage-account" title={account}>{account}</p>}
    {windows.length ? <div className="usage-windows">{windows.map((window) => <div className="usage-window" key={window.key}>
      <div className="usage-window-heading"><span>{window.label}</span><strong>{Math.round(window.remaining)}% <small>left</small></strong></div>
      <div className={`usage-meter${window.remaining <= 10 ? " low" : ""}`} role="meter" aria-label={`${window.label} remaining`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={window.remaining} aria-valuetext={`${Math.round(window.remaining)}% remaining`}><span style={{ width: `${window.remaining}%` }} /></div>
      <small className="usage-reset">{window.reset ? `Resets ${resetTime.format(window.reset)}` : "Reset time not reported"}</small>
    </div>)}</div> : <div className="usage-empty" role="status">{loading ? <><Spinner />Checking usage</> : error ? "Couldn’t check usage" : !provider?.authenticated ? "Sign in to check usage" : "Usage not reported by this provider"}</div>}
  </section>;
});
