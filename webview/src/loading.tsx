import type { WorkbenchSnapshot } from "./types";
import { LoadingState } from "./ai";
import { request } from "./bridge";
import { useState } from "react";

export function resourceState(snapshot: WorkbenchSnapshot | null, resource: "accounts" | "threads" | "models" | "repos") {
  if (!snapshot) return "loading";
  const items = resource === "accounts" ? snapshot.providerAccounts : resource === "models" ? snapshot.modelCatalog : snapshot[resource];
  if (items?.length) return "ready";
  if (snapshot.loadState === "error" || snapshot.error) return "error";
  if (snapshot.loadState === "loading") return "loading";
  if (resource === "accounts" || resource === "models") {
    if (snapshot.detectionState === "error") return "error";
    if (snapshot.detectionState === "idle" || snapshot.detectionState === "loading") return "loading";
  }
  return "ready";
}
export function ResourceState({ state, label }: { state: "loading" | "error"; label: string }) {
  const [since] = useState(Date.now);
  return <div className="resource-state" role="status" aria-busy={state === "loading"}>{state === "loading" ? <><LoadingState label={`Loading ${label}`} since={since} /><div className="resource-skeleton" aria-hidden="true"><span /><span /><span /></div></> : <><span>Couldn’t load {label}.</span><button className="text-btn" onClick={() => { void request({ type: "refresh" }).catch(() => {}); }}>Retry</button></>}</div>;
}
