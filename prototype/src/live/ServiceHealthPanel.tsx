import { useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";

type Restart = { at_ms: number; reason: string; method?: string | null; phase?: string | null };
type Health = { restarts: Restart[]; diagnostics_enabled: boolean; log_path?: string | null };

/** Worker restarts in the last 24 hours with their causes, and the diagnostics
 *  log switch, so a recurring problem is visible without a terminal. */
export function ServiceHealthPanel({ now = () => Date.now() }: { now?: () => number }) {
  const [health, setHealth] = useState<Health | null>(null), [busy, setBusy] = useState(false), [error, setError] = useState("");
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    const read = () => { void invoke<Health>("worker_health").then(value => { if (active) setHealth(value); }, () => {}); };
    read(); const timer = window.setInterval(read, 10_000);
    return () => { active = false; window.clearInterval(timer); };
  }, []);
  if (!isTauri() || !health) return null;
  const day = now() - 86_400_000;
  const recent = health.restarts.filter(restart => restart.at_ms >= day).reverse();
  const toggle = async (enabled: boolean) => {
    setBusy(true); setError("");
    try { setHealth(await invoke<Health>("worker_diagnostics_set", { enabled })); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  };
  return <section className="service-health" aria-label="Service health">
    <h3>Service health</h3>
    <p>{recent.length === 0 ? "No station service restarts in the last 24 hours."
      : `${recent.length} station service restart${recent.length === 1 ? "" : "s"} in the last 24 hours.`}</p>
    {recent.length > 0 && <ul>{recent.slice(0, 10).map(restart => <li key={`${restart.at_ms}-${restart.method ?? ""}`}>
      {new Date(restart.at_ms).toLocaleTimeString()}: {restart.reason}{restart.method ? ` (after ${restart.method}${restart.phase ? `, ${restart.phase.replace(/_/g, " ")}` : ""})` : ""}
    </li>)}</ul>}
    <label><input type="checkbox" aria-label="Write a diagnostics log" checked={health.diagnostics_enabled} disabled={busy}
      onChange={event => void toggle(event.target.checked)} /> Write a diagnostics log (request timings and restarts; capped at two 20 MB files)</label>
    {health.diagnostics_enabled && health.log_path && <p className="service-health-path">Log: <code>{health.log_path}</code></p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}

export default ServiceHealthPanel;
