import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useId, useRef, useState, type CSSProperties, type ReactNode } from "react";
import type { ClusterPalette, LiveClusterSpot } from "./ClusterPanel";
import { ChaseAlertsTable } from "./ChaseAlertsTable";
import { CompactSpotTable } from "./CompactSpotTable";
import { defaultClusterFilter, normalizeClusterFilter, type ClusterFilter } from "./clusterFilters";
import { BandMapsToolbarContext } from "./BandMaps";
import ClusterFilterDialog from "./ClusterFilterDialog";
import { PushToPanButton } from "./PushToPanButton";
import { boundColumnWidth, compactClusterColumns, defaultWorkspacePreferences, operatorWorkspaceKey, readWorkspacePreferences, splitBounds,
  type CompactColumn, type WorkspacePreferences } from "./operatorWorkspacePreferences";
import { useClusterAppearancePreference } from "./clusterAppearance";
import "./OperatorWorkspace.css";

export type OperatorWorkspaceProps = { map: ReactNode; toolsOnly?: boolean; bandMaps: ReactNode; cluster: unknown;
  onSelect: (spot: LiveClusterSpot) => void; onRequest?: (method: string, params?: Record<string, unknown>) => Promise<unknown>;
  selectedSpotId?: string | null; palette?: ClusterPalette };
type ClusterProjection = { snapshot?: { spots?: LiveClusterSpot[]; connectedClusters?: number; connections?: { serverName: string; phase: string; error?: string | null }[] };
  configuration?: { palette?: ClusterPalette; filter?: ClusterFilter; specialRules?: { name: string; enabled: boolean }[] } };
export function OperatorWorkspace({ map, toolsOnly = false, bandMaps, cluster, onSelect, onRequest, selectedSpotId, palette }: OperatorWorkspaceProps) {
  const { appearance } = useClusterAppearancePreference();
  const classic = appearance === "classic";
  const [preferences, setPreferences] = useState<WorkspacePreferences>(() => {
    try { return readWorkspacePreferences(window.localStorage.getItem(operatorWorkspaceKey)); } catch { return defaultWorkspacePreferences(); }
  });
  const [width, setWidth] = useState(0), [clusterHeight, setClusterHeight] = useState(0), [toolsHeight, setToolsHeight] = useState(0);
  const toolsStack = useRef<HTMLDivElement>(null), verticalDrag = useRef(false);
  const [bandToolbarHost, setBandToolbarHost] = useState<HTMLDivElement | null>(null);
  const [columnsOpen, setColumnsOpen] = useState(false), [filtersOpen, setFiltersOpen] = useState(false);
  const [filter, setFilter] = useState<ClusterFilter>(defaultClusterFilter), [filterError, setFilterError] = useState("");
  const [page, setPage] = useState(0), [columnPage, setColumnPage] = useState(0);
  const root = useRef<HTMLDivElement>(null), clusterPane = useRef<HTMLDivElement>(null);
  const filtersButton = useRef<HTMLButtonElement>(null), filterVersion = useRef(0);
  const id = useId();
  const drag = useRef(false);
  useEffect(() => {
    try { window.localStorage.setItem(operatorWorkspaceKey, JSON.stringify(preferences)); } catch { /* Restricted storage: keep this session usable. */ }
  }, [preferences]);
  useEffect(() => {
    const measure = () => { setWidth(root.current?.getBoundingClientRect().width ?? 0); setClusterHeight(clusterPane.current?.getBoundingClientRect().height ?? 0); setToolsHeight(toolsStack.current?.clientHeight || toolsStack.current?.getBoundingClientRect().height || 0); };
    measure();
    if (typeof ResizeObserver === "undefined") { window.addEventListener("resize", measure); return () => window.removeEventListener("resize", measure); }
    const observer = new ResizeObserver(measure);
    if (root.current) observer.observe(root.current);
    if (clusterPane.current) observer.observe(clusterPane.current);
    if (toolsStack.current) observer.observe(toolsStack.current);
    return () => observer.disconnect();
  }, [preferences.activeTab, preferences.showAlerts]);
  const toolsMinimum = toolsHeight > 210 ? Math.max(20, 100 / (toolsHeight - 10) * 100) : 50;
  const toolsMaximum = 100 - toolsMinimum;
  const toolsPercent = Math.max(toolsMinimum, Math.min(toolsMaximum, preferences.toolsPercent));
  const setToolsSplit = (value: number) => {
    if (Number.isFinite(value)) setPreferences(current => ({ ...current, toolsPercent: Math.max(toolsMinimum, Math.min(toolsMaximum, value)) }));
  };
  const bounds = splitBounds(width);
  const percent = Math.max(bounds.min, Math.min(bounds.max, preferences.mapPercent));
  const projection = cluster as ClusterProjection | null;
  const remoteFilter = projection?.configuration?.filter;
  const spots = Array.isArray(projection?.snapshot?.spots) ? projection.snapshot.spots : [];
  const colors = palette ?? projection?.configuration?.palette;
  const rows = Math.max(1, Math.min(50, Math.floor((clusterHeight - 68) / (classic ? 22 : 28)) || 1));
  const columnRows = Math.max(1, Math.min(7, Math.floor((clusterHeight - 93) / 26) || 1)), columnPageSize = columnRows * 2;
  const columnPages = Math.ceil(compactClusterColumns.length / columnPageSize), currentColumnPage = Math.min(columnPage, columnPages - 1);
  const pages = Math.max(1, Math.ceil(spots.length / rows)), currentPage = Math.min(page, pages - 1), offset = currentPage * rows;
  useEffect(() => { if (remoteFilter) setFilter(normalizeClusterFilter(remoteFilter)); }, [remoteFilter]);
  useEffect(() => {
    if (!filtersOpen) return;
    const close = (event: KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); setFiltersOpen(false); filtersButton.current?.focus(); } };
    document.addEventListener("keydown", close);
    return () => document.removeEventListener("keydown", close);
  }, [filtersOpen]);
  const changeFilter = (next: ClusterFilter) => {
    setFilter(next); setFilterError(""); setPage(0);
    if (!onRequest) return;
    const version = ++filterVersion.current;
    void onRequest("cluster_filter", { filter: next }).then(raw => {
      const accepted = (raw as ClusterProjection | null)?.configuration?.filter;
      if (version === filterVersion.current && accepted) setFilter(normalizeClusterFilter(accepted));
    }).catch(() => { if (version === filterVersion.current) setFilterError("Could not apply cluster filters."); });
  };
  const closeFilters = () => { setFiltersOpen(false); window.requestAnimationFrame(() => filtersButton.current?.focus()); };
  const specialChoices = ["RBN", ...(projection?.configuration?.specialRules?.filter(rule => rule.enabled && rule.name.toUpperCase() !== "RBN").map(rule => rule.name) ?? [])];
  const setSplit = (value: number) => setPreferences(current => ({ ...current, mapPercent: Math.max(bounds.min, Math.min(bounds.max, value)) }));
  const setColumnWidth = (column: CompactColumn, value: number) => {
    if (!Number.isFinite(value)) return;
    setPreferences(current => ({ ...current, widths: { ...current.widths, [column]: boundColumnWidth(value) } }));
  };
  const move = (clientX: number) => {
    if (drag.current && width > 740) setSplit((clientX - (root.current?.getBoundingClientRect().left ?? 0)) / (width - 10) * 100);
  };
  return <div className={`operator-workspace${toolsOnly ? " tools-only" : ""}${width > 0 && width <= 740 ? " is-stacked" : ""}`} ref={root} style={{ "--operator-map-share": `${percent}%` } as CSSProperties}>
    {!toolsOnly && <><div className="operator-map-pane">{map}</div>
    <div className="operator-pane-divider" role="separator" aria-label="Map and tools width" aria-orientation="vertical" tabIndex={0}
      aria-valuemin={Math.round(bounds.min)} aria-valuemax={Math.round(bounds.max)} aria-valuenow={Math.round(percent)} aria-valuetext={`${Math.round(percent)}% map`}
      onKeyDown={event => { const value = event.key === "Home" ? bounds.min : event.key === "End" ? bounds.max : event.key === "ArrowLeft" ? percent - 2 : event.key === "ArrowRight" ? percent + 2 : null;
        if (value != null) { event.preventDefault(); setSplit(value); } }}
      onPointerDown={event => { if (event.button !== 0) return; event.preventDefault(); drag.current = true; event.currentTarget.setPointerCapture?.(event.pointerId); move(event.clientX); }}
      onPointerMove={event => move(event.clientX)} onPointerUp={() => { drag.current = false; }} onPointerCancel={() => { drag.current = false; }} onLostPointerCapture={() => { drag.current = false; }} /></>}
    <div className="operator-tools-pane">
      <div className="operator-tool-toolbar"><label className="operator-tools-view" htmlFor={`${id}-view`}>View
        <select id={`${id}-view`} aria-label="Desk tools view" value={`${preferences.activeTab}${preferences.showAlerts ? "-alerts" : ""}`} onChange={event => {
          const view = event.currentTarget.value;
          if (!["band-maps", "cluster", "band-maps-alerts", "cluster-alerts"].includes(view)) return;
          verticalDrag.current = false;
          setPreferences(current => ({ ...current, activeTab: view.startsWith("band-maps") ? "band-maps" : "cluster", showAlerts: view.endsWith("-alerts") }));
        }}><option value="band-maps">Band maps</option><option value="cluster">Cluster</option>
          <option value="band-maps-alerts">Band maps + Alerts</option><option value="cluster-alerts">Cluster + Alerts</option></select></label>
        <div ref={setBandToolbarHost} className="operator-band-map-toolbar" hidden={preferences.activeTab !== "band-maps"} />
        <div className="operator-cluster-toolbar" hidden={preferences.activeTab !== "cluster"}>
          <div className="operator-cluster-connections" role="status" aria-label="Compact cluster connections">{projection?.snapshot?.connections?.length ? projection.snapshot.connections.map((connection, index) => {
            const phase = connection.phase.toLowerCase(), label = phase.charAt(0).toUpperCase() + phase.slice(1);
            return <span key={`${connection.serverName}-${index}`} className={`operator-cluster-connection ${phase === "connected" ? "is-connected" : ["disconnected", "failed"].includes(phase) ? "is-disconnected" : "is-connecting"}`}>
              {connection.serverName} · {label}{connection.error && <ErrorNotice when={true}><small> · {connection.error}</small></ErrorNotice>}</span>;
          }) : <span className={`operator-cluster-connection ${projection?.snapshot ? projection.snapshot.connectedClusters ? "is-connected" : "is-disconnected" : "is-connecting"}`}>{projection?.snapshot ? projection.snapshot.connectedClusters ? `${projection.snapshot.connectedClusters} connected` : "Disconnected" : "Awaiting live cluster state"}</span>}</div>
          <button ref={filtersButton} type="button" aria-expanded={filtersOpen} aria-controls={`${id}-filters`}
            disabled={!onRequest || !remoteFilter} onClick={() => { setColumnsOpen(false); setFiltersOpen(true); }}>Filters</button>
          <PushToPanButton cluster={cluster} onRequest={onRequest} />
          <button type="button" aria-expanded={columnsOpen} aria-controls={`${id}-columns`} onClick={() => { setFiltersOpen(false); setColumnsOpen(value => !value); }}>Columns</button>
        </div>
      </div>
      {filtersOpen && <ClusterFilterDialog dialogID={`${id}-filters`} filter={filter} specialChoices={specialChoices} error={filterError}
        onChange={changeFilter} onClose={closeFilters} />}
      <div ref={toolsStack} className={`operator-tools-stack${preferences.showAlerts ? " has-alerts" : ""}`} style={{ gridTemplateRows: preferences.showAlerts ? `minmax(0, ${toolsPercent}fr) 10px minmax(0, ${100 - toolsPercent}fr)` : "minmax(0, 1fr)" }}>
      <div className="operator-primary-tools">
      <div id={`${id}-band-maps`} role="region" aria-label="Band maps" className="operator-tool-content" hidden={preferences.activeTab !== "band-maps"}><BandMapsToolbarContext.Provider value={bandToolbarHost}>{bandMaps}</BandMapsToolbarContext.Provider></div>
      <div id={`${id}-cluster`} role="region" aria-label="Cluster" className={`operator-tool-content operator-compact-cluster${classic ? " is-classic" : ""}`} ref={clusterPane} hidden={preferences.activeTab !== "cluster"}>
        <div id={`${id}-columns`} className="operator-column-settings" hidden={!columnsOpen}>
          <p>Preferred widths scale to fit. Drag a header edge or use its arrow keys.</p>
          <div>{compactClusterColumns.slice(currentColumnPage * columnPageSize, (currentColumnPage + 1) * columnPageSize).map(([column, label]) => <label key={column}><input type="checkbox" aria-label={`Show ${label} column`} checked={preferences.columns.includes(column)} disabled={preferences.columns.length === 1 && preferences.columns.includes(column)}
            onChange={event => { const checked = event.target.checked; setPreferences(current => ({ ...current, columns: compactClusterColumns.map(([value]) => value).filter(value => value === column ? checked : current.columns.includes(value)) })); }} />
            <span title={label}>{label}</span><input key={`${column}-${preferences.widths[column]}`} type="number" aria-label={`${label} preferred width`} min={48} max={480} defaultValue={preferences.widths[column]}
              onBlur={event => { const value = event.target.value ? Number(event.target.value) : preferences.widths[column]; setColumnWidth(column, value); event.target.value = String(Number.isFinite(value) ? boundColumnWidth(value) : preferences.widths[column]); }}
              onKeyDown={event => { if (event.key === "Enter") event.currentTarget.blur(); }} /></label>)}</div>
          <nav aria-label="Column settings pages"><button type="button" disabled={currentColumnPage === 0} aria-label="Previous column settings page" onClick={() => setColumnPage(currentColumnPage - 1)}>‹</button>
            <span>{currentColumnPage + 1} / {columnPages}</span><button type="button" disabled={currentColumnPage + 1 >= columnPages} aria-label="Next column settings page" onClick={() => setColumnPage(currentColumnPage + 1)}>›</button>
            <button type="button" onClick={() => { const defaults = defaultWorkspacePreferences(); setPreferences(current => ({ ...current, columns: defaults.columns, widths: defaults.widths })); }}>Reset columns and widths</button></nav>
        </div>
        <CompactSpotTable spots={spots.slice(offset, offset + rows)} preferences={preferences} onWidth={setColumnWidth} onSelect={onSelect}
          selectedSpotId={selectedSpotId} colors={colors} classic={classic} hidden={columnsOpen}
          emptyMessage={projection?.snapshot ? "No spots in the current live cluster view." : "Awaiting live cluster state."} />
        <footer hidden={columnsOpen}><button type="button" aria-label="Previous spots page" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>‹</button>
          <span>{spots.length ? `${offset + 1}–${Math.min(offset + rows, spots.length)} of ${spots.length}` : "0 spots"}</span>
          <button type="button" aria-label="Next spots page" disabled={currentPage + 1 >= pages} onClick={() => setPage(currentPage + 1)}>›</button></footer>
      </div>
      </div>
      {preferences.showAlerts && <><div className="operator-alerts-divider" role="separator" aria-label="Tools and Alerts height" aria-orientation="horizontal" tabIndex={toolsMinimum === toolsMaximum ? -1 : 0}
        aria-valuemin={Math.round(toolsMinimum)} aria-valuemax={Math.round(toolsMaximum)} aria-valuenow={Math.round(toolsPercent)} aria-valuetext={`${Math.round(toolsPercent)}% ${preferences.activeTab === "cluster" ? "Cluster" : "Band maps"}`}
        onKeyDown={event => { const step = event.shiftKey ? 10 : 2;
          const next = event.key === "Home" ? toolsMinimum : event.key === "End" ? toolsMaximum : event.key === "ArrowUp" ? toolsPercent - step : event.key === "ArrowDown" ? toolsPercent + step : null;
          if (next !== null) { event.preventDefault(); setToolsSplit(next); } }}
        onPointerDown={event => { if (event.button !== 0 || toolsMinimum === toolsMaximum) return; event.preventDefault(); event.currentTarget.focus(); event.currentTarget.setPointerCapture?.(event.pointerId); verticalDrag.current = true; }}
        onPointerMove={event => { if (!verticalDrag.current) return; const box = toolsStack.current?.getBoundingClientRect(); if (box && box.height > 10) setToolsSplit((event.clientY - box.top) / box.height * 100); }}
        onPointerUp={() => { verticalDrag.current = false; }} onPointerCancel={() => { verticalDrag.current = false; }} onLostPointerCapture={() => { verticalDrag.current = false; }} />
        <div className="operator-alerts-pane"><ChaseAlertsTable preferences={preferences} onWidth={setColumnWidth} colors={colors} /></div></>}
      </div>
    </div>
  </div>;
}
