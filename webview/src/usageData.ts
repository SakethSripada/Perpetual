import type { ProviderUsage } from "./types";

export function usageWindows(usage?: ProviderUsage | null) {
  return (["five_hour", "weekly"] as const).flatMap((key) => {
    const window = usage?.[key];
    if (!window || !Number.isFinite(window.used_percent)) return [];
    const used = Math.min(100, Math.max(0, window.used_percent));
    const date = window.reset_at ? new Date(window.reset_at) : null;
    return [{ key, label: key === "five_hour" ? "5-hour limit" : "Weekly limit", used, remaining: 100 - used,
      reset: date && Number.isFinite(date.getTime()) ? date : null }];
  });
}
