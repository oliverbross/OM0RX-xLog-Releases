import { useEffect, useState } from "react";

export type LogbookAppearance = "modern" | "classic";
export const logbookAppearanceKey = "xlog.display.logbook-appearance.v1";
const changed = "xlog:logbook-appearance";
export function readLogbookAppearance(): LogbookAppearance {
  try { return localStorage.getItem(logbookAppearanceKey) === "classic" ? "classic" : "modern"; }
  catch { return "modern"; }
}
export function useLogbookAppearancePreference() {
  const [appearance, setAppearance] = useState(readLogbookAppearance);
  const [error, setError] = useState("");
  useEffect(() => {
    const refresh = () => setAppearance(readLogbookAppearance());
    const stored = (event: StorageEvent) => { if (event.key === logbookAppearanceKey || event.key === null) refresh(); };
    window.addEventListener("storage", stored);
    window.addEventListener(changed, refresh);
    window.addEventListener("focus", refresh);
    return () => { window.removeEventListener("storage", stored); window.removeEventListener(changed, refresh); window.removeEventListener("focus", refresh); };
  }, []);
  const save = (value: LogbookAppearance) => {
    if (value !== "modern" && value !== "classic") return;
    try { localStorage.setItem(logbookAppearanceKey, value); setAppearance(value); setError(""); window.dispatchEvent(new Event(changed)); }
    catch { setError("Could not save the logbook appearance. Your previous style is still active; try again."); }
  };
  return { appearance, save, error };
}
