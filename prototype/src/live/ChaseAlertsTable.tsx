import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { AppDialog } from "./AppDialog";
import { AlertReview } from "./DetachedAlertsSurface";
import { CompactSpotTable } from "./CompactSpotTable";
import type { ClusterPalette, LiveClusterSpot } from "./ClusterPanel";
import { connectDxAlerts, reviewDxAlert, type DxAlertEntry, type DxAlertsSnapshot } from "./dxAlerts";
import { boundColumnWidth, compactClusterColumns, operatorWorkspaceKey, readWorkspacePreferences, type CompactColumn, type WorkspacePreferences } from "./operatorWorkspacePreferences";
import { useClusterAppearancePreference } from "./clusterAppearance";
import { useSpotActivation } from "./useSpotActivation";
import { SpotActivationStatus } from "./SpotActivationStatus";

export function alertSpot(entry: DxAlertEntry): LiveClusterSpot {
  return { id: entry.id, receivedAt: entry.lastSeenMs / 1000 - 978307200, clusterName: "Alerts", spotter: entry.spotter ?? "", frequencyKHz: entry.frequencyKHz,
    frequencyText: String(entry.frequencyKHz), callsign: entry.callsign, comment: entry.comment, band: entry.band, mode: entry.mode, country: entry.country ?? "",
    continent: entry.geography?.dxContinent ?? "", cqZone: entry.geography?.dxCqZone, adif: entry.geography?.dxEntity, callStatus: entry.callStatus, dxccStatus: entry.dxccStatus,
    workedBefore: entry.workedBefore, specialTags: entry.specialTags, score: entry.score ?? 0, scoreReasons: [], aggregateCount: entry.count, aggregateSpotters: entry.spotter ? [entry.spotter] : [],
    source: entry.source ?? "cluster", statusHex: "", bandHex: "", modeHex: "" };
}
type Columns = Pick<WorkspacePreferences, "columns" | "widths">;
const key = "xlog.integration.alert-table-columns.v1";
export function ChaseAlertsTable({ snapshot: supplied, preferences: shared, onWidth, colors }: {
  snapshot?: DxAlertsSnapshot | null; preferences?: Columns; onWidth?: (column: CompactColumn, width: number) => void; colors?: ClusterPalette;
}) {
  const [received, setReceived] = useState<DxAlertsSnapshot | null>(null), [error, setError] = useState("");
  const [preferences, setPreferences] = useState(() => {
    try {
      const raw = localStorage.getItem(key) ?? localStorage.getItem(operatorWorkspaceKey);
      return raw ? readWorkspacePreferences(raw) : { ...readWorkspacePreferences(null), columns: compactClusterColumns.map(([id]) => id) };
    } catch { return readWorkspacePreferences(null); }
  });
  const [height, setHeight] = useState(0), [page, setPage] = useState(0);
  const body = useRef<HTMLDivElement>(null);
  const [review, setReview] = useState<Awaited<ReturnType<typeof reviewDxAlert>> | null>(null);
  const activation = useSpotActivation(), selecting = useRef(false);
  const [requested, setRequested] = useState(false);
  const { appearance } = useClusterAppearancePreference(), classic = appearance === "classic";
  useEffect(() => supplied === undefined ? connectDxAlerts(setReceived, setError) : undefined, [supplied === undefined]);
  useLayoutEffect(() => {
    const measure = () => setHeight(body.current?.clientHeight || body.current?.getBoundingClientRect().height || 0);
    measure(); if (!body.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure); observer.observe(body.current); return () => observer.disconnect();
  }, []);
  const snapshot = supplied === undefined ? received : supplied;
  const entries = snapshot?.entries ?? [], rows = Math.max(1, Math.min(200, Math.floor((height - 60) / (classic ? 22 : 28)) || 1));
  const current = Math.min(page, Math.max(0, Math.ceil(entries.length / rows) - 1)), offset = current * rows;
  const resize = (column: CompactColumn, width: number) => {
    if (shared && onWidth) { onWidth(column, width); return; }
    setPreferences(value => {
      const next = { ...value, widths: { ...value.widths, [column]: boundColumnWidth(width) } };
      try { localStorage.setItem(key, JSON.stringify(next)); } catch { /* Keep this session usable. */ }
      return next;
    });
  };
  const open = async (spot: LiveClusterSpot) => {
    if (!snapshot || selecting.current || !activation.canActivate()) return;
    selecting.current = true;
    try {
      const value = await reviewDxAlert(snapshot.epoch, spot.id);
      setError("");
      const observation = value.currentObservation ?? value.observation;
      if (value.admissible && !value.entry.test && value.entry.source === "cluster" && observation) {
        setRequested(true); await activation.activate(observation);
      } else { setRequested(false); setReview(value); }
    }
    catch (cause) { setError(cause instanceof Error ? cause.message : "Alert review unavailable."); }
    finally { selecting.current = false; }
  };
  return <div ref={body} className={`operator-tool-content operator-compact-cluster chase-alert-table${classic ? " is-classic" : ""}`} aria-label="Alerts table">
    {(error || snapshot?.storageError) && <ErrorNotice><p role="alert" className="chase-form-error">{error || snapshot?.storageError}</p></ErrorNotice>}
    {requested && <SpotActivationStatus {...activation}/>}
    <CompactSpotTable name="Alert reports" preferences={shared ?? preferences} onWidth={resize} spots={entries.slice(offset, offset + rows).map(alertSpot)} onSelect={spot => void open(spot)}
      colors={colors} classic={classic} alertMenu={false} emptyMessage={!snapshot ? "Loading alerts…" : !snapshot.configuration.enabled ? "Alerts are paused. Save a target or enable Alerts in Settings." : "No needed alerts right now. No matching saved callsigns in the current cluster."} />
    <footer><button type="button" aria-label="Previous alerts page" disabled={!current} onClick={() => setPage(current - 1)}>‹</button>
      <span>{entries.length ? `${offset + 1}–${Math.min(offset + rows, entries.length)} of ${entries.length}` : "0 alerts"}</span>
      <button type="button" aria-label="Next alerts page" disabled={offset + rows >= entries.length} onClick={() => setPage(current + 1)}>›</button></footer>
    {review && <AppDialog title={`Alert · ${review.entry.callsign}`} onClose={() => setReview(null)}><AlertReview review={review} onClose={() => setReview(null)} onError={setError} /></AppDialog>}
  </div>;
}
