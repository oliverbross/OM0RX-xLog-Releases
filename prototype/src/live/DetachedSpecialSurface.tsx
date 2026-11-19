import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Settings } from "lucide-react";
import { workerRequest } from "../worker";
import { AppDialog } from "./AppDialog";
import { CompactSpotTable } from "./CompactSpotTable";
import type { LiveClusterSpot } from "./ClusterPanel";
import { announceDetachedClusterReady, newerClusterProjection, onClusterProjection, type ClusterProjection } from "./DetachedClusterBridge";
import { useClusterAppearancePreference } from "./clusterAppearance";
import { connectLogbookChanges } from "./logbookWindows";
import { useSpotActivation } from "./useSpotActivation";
import { SpotActivationStatus } from "./SpotActivationStatus";
import { boundColumnWidth, compactClusterColumns, type CompactColumn } from "./operatorWorkspacePreferences";
import { distinctSpecialReports, specialKey, specialPreferences, specialProgrammes, specialProgress, type SpecialSnapshot } from "./specialStations";
import "./SpecialSurface.css";

export async function readSpecialSnapshot(): Promise<SpecialSnapshot> {
  const value = (await workerRequest("opportunities_special_snapshot", {})).result as SpecialSnapshot;
  if (!value || typeof value.history_available !== "boolean" || !Array.isArray(value.spots) || value.spots.length > 500 || value.spots.some(row =>
    !row || typeof row.id !== "string" || typeof row.callsign !== "string" || !Number.isFinite(row.frequencyKHz) || typeof row.comment !== "string" || typeof row.band !== "string" || typeof row.mode !== "string" ||
    !Array.isArray(row.programmes) || row.programmes.some(kind => !specialProgrammes.some(([id]) => id === kind)) || !Array.isArray(row.references) || row.references.some(ref =>
      !ref || typeof ref.kind !== "string" || typeof ref.reference !== "string" || !["new", "worked", "unknown"].includes(ref.status)))) throw Error("Special station history could not be verified.");
  return value;
}
export function DetachedSpecialSurface() {
  const [preferences, setPreferences] = useState(() => { try { return specialPreferences(localStorage.getItem(specialKey)); } catch { return specialPreferences(null); } });
  const [projection, setProjection] = useState<ClusterProjection | null>(null), latest = useRef<ClusterProjection | null>(null);
  const [snapshot, setSnapshot] = useState<SpecialSnapshot | null>(null), [error, setError] = useState("");
  const [settings, setSettings] = useState(false), [columns, setColumns] = useState(false), [page, setPage] = useState(0), [height, setHeight] = useState(0);
  const body = useRef<HTMLDivElement>(null), activation = useSpotActivation();
  const { appearance } = useClusterAppearancePreference(), classic = appearance === "classic";
  useEffect(() => { try { localStorage.setItem(specialKey, JSON.stringify(preferences)); } catch { /* Retain session preferences. */ } }, [preferences]);
  useEffect(() => {
    let active = true, reading = false, pending = false, failures = 0;
    const read = async () => {
      if (reading) { pending = true; return; }
      reading = true;
      try { const value = await readSpecialSnapshot(); failures = 0; if (active) { setSnapshot(value); setError(""); } }
      catch (cause) {
        // One slow read keeps the last table; only repeated failures say so.
        failures += 1;
        if (active && failures >= 3) setError(cause instanceof Error ? cause.message : "Special station history unavailable.");
      }
      finally { reading = false; if (active && pending) { pending = false; void read(); } }
    };
    let stop: (() => void) | undefined;
    void onClusterProjection(next => { if (active && newerClusterProjection(latest.current, next) === next) { latest.current = next; setProjection(next); } })
      .then(dispose => { if (!active) { dispose(); return; } stop = dispose; void announceDetachedClusterReady().catch(cause => { if (active) setError(String(cause)); }); })
      .catch(() => { if (active) setError("Cluster updates unavailable. Reopen Special to retry."); });
    const stopHistory = connectLogbookChanges(() => void read(), setError);
    void read(); const timer = setInterval(() => void read(), 5000);
    return () => { active = false; clearInterval(timer); stop?.(); stopHistory(); };
  }, []);
  useLayoutEffect(() => {
    const measure = () => setHeight(body.current?.clientHeight ?? 0); measure();
    if (!body.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure); observer.observe(body.current); return () => observer.disconnect();
  }, []);
  const response = projection?.response as { snapshot?: { spots?: LiveClusterSpot[] }; contest_spots?: LiveClusterSpot[] } | null;
  const spots = response?.contest_spots ?? response?.snapshot?.spots ?? [];
  const reports = distinctSpecialReports(spots, snapshot?.spots ?? []).flatMap(({ spot, row }) => {
    if (!row.programmes.some(kind => preferences.programmes.includes(kind))) return [];
    const detail = specialProgress(spot, row, preferences.programmes, snapshot?.history_available === true);
    return preferences.progress === "new" && !detail.isNew || preferences.progress === "worked" && !detail.worked ? [] : [{ spot, ...detail }];
  });
  const rows = Math.max(1, Math.floor((height - 60) / (classic ? 22 : 28)) || 1), current = Math.min(page, Math.max(0, Math.ceil(reports.length / rows) - 1)), offset = current * rows;
  const labels: Partial<Record<CompactColumn, string>> = { special: "Reference", workedBefore: "Progress" };
  const width = (column: CompactColumn, value: number) => setPreferences(prev => ({ ...prev, widths: { ...prev.widths, [column]: boundColumnWidth(value) } }));
  return <div className="app live monitor-app special-app"><main aria-label="Special stations">
    <header><h1>Special</h1><div><button type="button" onClick={() => setColumns(true)}>Columns</button><button type="button" aria-label="Special filters" title="Special filters" onClick={() => setSettings(true)}><Settings size={16} aria-hidden="true"/></button></div></header>
    <div className="special-summary"><span>{reports.length} reports · {preferences.progress === "new" ? "New only" : preferences.progress === "worked" ? "Worked before" : "All stations"}</span><span>{preferences.programmes.map(kind => specialProgrammes.find(([id]) => id === kind)?.[1]).join(" · ") || "No programmes selected"}</span></div>
    {error && <ErrorNotice><p role="alert" className="live-error">{error}</p></ErrorNotice>}
    {snapshot && !snapshot.history_available && <ErrorNotice><p role="status">Reference history unavailable. New references cannot be determined.</p></ErrorNotice>}
    {projection && !projection.available && <ErrorNotice><p role="status">Cluster updates paused. Retained reports remain visible.</p></ErrorNotice>}
    <SpotActivationStatus {...activation}/>
    <div ref={body} className={`operator-compact-cluster special-table${classic ? " is-classic" : ""}`}>
      <CompactSpotTable name="Special station reports" spots={reports.slice(offset, offset + rows).map(row => row.spot)} preferences={preferences} onWidth={width} classic={classic} alertMenu={false}
        columnOverrides={{ special: { label: "Reference", value: spot => reports.find(row => row.spot.id === spot.id)?.reference ?? "—" }, workedBefore: { label: "Progress", value: spot => reports.find(row => row.spot.id === spot.id)?.progress ?? "Unknown" } }}
        emptyMessage={!snapshot || !projection ? "Loading special stations…" : "No special stations match these filters."}
        onSelect={spot => { const source = latest.current, age = Date.now() - (source?.received_at_ms ?? 0);
          if (!source?.available || age > 5000 || age < -1000) { setError("Refresh cluster state before selecting a special station."); return; }
          setError(""); void activation.activate(spot).catch(cause => setError(cause instanceof Error ? cause.message : "Station selection unavailable.")); }}/>
      <footer><button type="button" aria-label="Previous special page" disabled={!current} onClick={() => setPage(current - 1)}>‹</button><span>{reports.length ? `${offset + 1}–${Math.min(offset + rows, reports.length)} of ${reports.length}` : "0 reports"}</span><button type="button" aria-label="Next special page" disabled={offset + rows >= reports.length} onClick={() => setPage(current + 1)}>›</button></footer>
    </div>
    {settings && <AppDialog title="Special filters" onClose={() => setSettings(false)}><div className="special-options">
      <fieldset><legend>Programmes</legend><div>{specialProgrammes.map(([id, label]) => <button type="button" key={id} aria-pressed={preferences.programmes.includes(id)} onClick={() => { setPage(0); setPreferences(prev => ({ ...prev, programmes: prev.programmes.includes(id) ? prev.programmes.filter(value => value !== id) : [...prev.programmes, id] })); }}>{label}</button>)}</div></fieldset>
      <label>Show<select aria-label="Special progress filter" value={preferences.progress} onChange={event => { setPage(0); setPreferences(prev => ({ ...prev, progress: event.target.value as typeof prev.progress })); }}><option value="all">All stations</option><option value="new">New only</option><option value="worked">Worked before</option></select></label>
      <p>New only includes a new callsign or an unworked activation reference. A worked operator at a new park stays new. Missing references remain unknown.</p>
    </div></AppDialog>}
    {columns && <AppDialog title="Special columns" onClose={() => setColumns(false)}><div className="special-column-options">{compactClusterColumns.map(([id, label]) => <label key={id}><input type="checkbox" checked={preferences.columns.includes(id)} disabled={id === "callsign"} onChange={event => setPreferences(prev => ({ ...prev, columns: event.target.checked ? [...prev.columns, id] : prev.columns.filter(column => column !== id) }))}/>{labels[id] ?? label}</label>)}</div><p>Drag a column divider to resize it.</p><button type="button" onClick={() => setPreferences(prev => ({ ...prev, columns: specialPreferences(null).columns, widths: specialPreferences(null).widths }))}>Reset columns</button></AppDialog>}
  </main></div>;
}
