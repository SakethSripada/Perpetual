import { useEffect } from "react";

/** Keep keyboard navigation inside open sheets and restore the invoking control. */
export function useSheetAccessibility(open: boolean) {
  useEffect(() => {
    if (!open) return;
    const sheets = [...document.querySelectorAll<HTMLElement>(".sheet")];
    const sheet = sheets.reverse().find((element) => element.offsetParent !== null);
    if (!sheet) return;
    const previous = document.activeElement as HTMLElement | null;
    const controls = () => [...sheet.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex="0"]')].filter((element) => element.offsetParent !== null);
    controls()[0]?.focus();
    const key = (event: KeyboardEvent) => {
      // Nested menus own their keyboard handling, including menus portaled outside the sheet.
      if (event.defaultPrevented || (event.target instanceof Element && event.target.closest('[role="menu"], [role="listbox"], .popover'))) return;
      if (event.key === "Escape") {
        if (sheet.getAttribute("aria-busy") === "true" || sheet.querySelector('[aria-busy="true"]')) return;
        event.preventDefault(); event.stopPropagation();
        sheet.querySelector<HTMLButtonElement>('button[title="Close"]')?.click();
      }
      if (event.key !== "Tab") return;
      const items = controls();
      const first = items[0], last = items[items.length - 1];
      if (!first) return;
      if (event.shiftKey && (document.activeElement === first || !sheet.contains(document.activeElement))) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && (document.activeElement === last || !sheet.contains(document.activeElement))) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", key, true);
    return () => { document.removeEventListener("keydown", key, true); if (previous?.isConnected) previous.focus(); };
  }, [open]);
}

