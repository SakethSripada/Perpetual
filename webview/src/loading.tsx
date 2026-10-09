import type { WorkbenchSnapshot } from "./types";
import { request } from "./bridge";
import type { CSSProperties } from "react";

/** Radix Themes' eight-leaf spinner, adapted under MIT; no theme runtime needed. */
export function Spinner() {
  return <span className="resource-spinner" aria-hidden="true">{Array.from({ length: 8 }, (_, index) => <span key={index} style={{ transform: `rotate(${index * 45}deg)`, animationDelay: `${-800 + index * 100}ms` } as CSSProperties} />)}</span>;
}

export function resourceState(snapshot: WorkbenchSnapshot | null, resource: "accounts" | "threads" | "models" | "repos") {
  if (!snapshot) return "loading";
  const items = resource === "accounts" ? snapshot.providerAccounts : resource === "models" ? snapshot.modelCatalog : snapshot[resource];
  if (items?.length) return "ready";
  if (snapshot.loadState === "error" || snapshot.error) return "error";
  if (snapshot.loadState === "loading") return "loading";
  if (resource === "accounts" || resource === "models") {
    const detection = (resource === "accounts" ? snapshot.accountDetectionState : snapshot.modelDetectionState) ?? snapshot.detectionState;
    if (detection === "error") return "error";
    if (detection === "idle" || detection === "loading") return "loading";
  }
  return "ready";
}
export function ResourceState({ state, label }: { state: "loading" | "error"; label: string }) {
  return <div className="resource-state" role="status" aria-busy={state === "loading"}>{state === "loading" ? <div className="resource-loading"><Spinner /><span>Loading {label}</span></div> : <><span>Couldn’t load {label}.</span><button className="text-btn" onClick={() => { void request({ type: "refresh" }).catch(() => {}); }}>Retry</button></>}</div>;
}
