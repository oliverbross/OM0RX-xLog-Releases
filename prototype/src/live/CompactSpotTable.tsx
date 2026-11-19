import { useRef } from "react";
import type { ReactNode } from "react";
import { formatSpotFrequency } from "../spotFrequency";
import { clusterRowTint, clusterSpotTags, type ClusterPalette, type LiveClusterSpot } from "./ClusterPanel";
import { clusterBandColors, clusterModeColors, clusterStatusCode, clusterStatusColors, paletteColor, specialTagColors } from "./clusterPalette";
import { compactClusterColumns, compactClusterHeader, type CompactColumn, type WorkspacePreferences } from "./operatorWorkspacePreferences";
import { formatSpotAge, spotAgeColor, useSpotAgeClock } from "./spotAge";
import { countryDisplay } from "./countryDisplay";
import { useClusterAlertMenu } from "./useClusterAlertMenu";
import "./OperatorWorkspace.css";

const spotTags = (spot: LiveClusterSpot): string[] => clusterSpotTags(spot);
function spotText(column: CompactColumn, spot: LiveClusterSpot, now: number): string {
  switch (column) {
    case "time": return formatSpotAge(spot.receivedAt, now);
    case "frequency": return formatSpotFrequency(spot.frequencyKHz * 1000);
    case "cq": return String(spot.cqZone ?? "—");
    case "country": return countryDisplay(spot.country);
    case "spotter": return `${spot.spotter}${spot.aggregateCount > 1 ? ` +${spot.aggregateCount - 1}` : ""}`;
    case "special": return spotTags(spot).length ? spotTags(spot).join(" · ") : "—";
    case "workedBefore": return spot.workedBefore == null ? "—" : `✓ ${new Date((spot.workedBefore + 978307200) * 1000).toLocaleString()}`;
    default: return String(spot[column] || "—");
  }
}
function cell(column: CompactColumn, spot: LiveClusterSpot, now: number, palette?: ClusterPalette, classic = false): ReactNode {
  if (column === "time") return <span className="live-cluster-age live-cluster-age-strip" style={{ backgroundColor: classic ? undefined : spotAgeColor(spot.receivedAt, now) }}>{spotText(column, spot, now)}</span>;
  if (column === "band" || column === "mode") return <span className="live-cluster-category" style={{ color: paletteColor(spot[column], palette?.[column], column === "band" ? clusterBandColors : clusterModeColors) }}>{spot[column]}</span>;
  if (column === "callStatus" || column === "dxccStatus") return <span className="live-cluster-status" style={{ color: paletteColor(spot[column], palette?.status, clusterStatusColors) }}>{clusterStatusCode(spot[column])}</span>;
  if (column === "score") return <span className={`live-cluster-score ${spot.score >= 80 ? "priority" : spot.score >= 55 ? "high" : spot.score >= 30 ? "medium" : "low"}`}>{spot.score}</span>;
  if (column === "special") return spotTags(spot).length ? spotTags(spot).slice(0, 2).map(tag => <span key={tag} className="live-cluster-special-tag" style={{ ...specialTagColors(tag), ...(classic ? { backgroundColor: "transparent" } : {}) }}>{tag}</span>) : "—";
  return spotText(column, spot, now);
}

export function CompactSpotTable({ spots, preferences, onWidth, onSelect, selectedSpotId, colors, classic = false, hidden = false, name = "Compact live cluster", emptyMessage = "No matching reports.", alertMenu = true, columnOverrides }: {
  spots: LiveClusterSpot[]; preferences: Pick<WorkspacePreferences, "columns" | "widths">;
  onWidth: (column: CompactColumn, width: number) => void; onSelect: (spot: LiveClusterSpot) => void;
  selectedSpotId?: string | null; colors?: ClusterPalette; classic?: boolean; hidden?: boolean;
  name?: string; emptyMessage?: string; alertMenu?: boolean;
  columnOverrides?: Partial<Record<CompactColumn, { label: string; value: (spot: LiveClusterSpot) => string }>>;
}) {
  const now = useSpotAgeClock(), menu = useClusterAlertMenu(alertMenu);
  const drag = useRef<{ x: number; width: number; total: number; pixels: number } | null>(null);
  const totalWidth = preferences.columns.reduce((sum, column) => sum + preferences.widths[column], 0);
  const resize = (column: CompactColumn, x: number) => {
    const start = drag.current; if (!start) return;
    const fraction = Math.max(.01, Math.min(.99, (start.width / start.total * start.pixels + x - start.x) / start.pixels));
    if (start.total > start.width) onWidth(column, fraction * (start.total - start.width) / (1 - fraction));
  };
  return <><div className="operator-compact-table" hidden={hidden}><table aria-label={name}>
    <colgroup>{preferences.columns.map(column => <col key={column} style={{ width: `${preferences.widths[column] / totalWidth * 100}%` }} />)}</colgroup>
    <thead><tr>{preferences.columns.map(column => {
      const label = columnOverrides?.[column]?.label ?? compactClusterColumns.find(([id]) => id === column)![1];
      return <th key={column} aria-label={column === "callStatus" || column === "dxccStatus" ? label : undefined}
        className={column === "callStatus" || column === "dxccStatus" ? "live-cluster-status-column" : undefined}><span title={label}>{columnOverrides?.[column]?.label ?? compactClusterHeader(column)}</span>
        <span role="separator" tabIndex={0} aria-orientation="vertical" aria-label={`Resize ${label} column`} aria-valuemin={48} aria-valuemax={480} aria-valuenow={preferences.widths[column]}
          onKeyDown={event => { const value = event.key === "Home" ? 48 : event.key === "End" ? 480 : event.key === "ArrowLeft" ? preferences.widths[column] - 8 : event.key === "ArrowRight" ? preferences.widths[column] + 8 : null;
            if (value !== null) { event.preventDefault(); onWidth(column, value); } }}
          onPointerDown={event => { if (event.button !== 0) return; event.preventDefault(); event.currentTarget.focus();
            drag.current = { x: event.clientX, width: preferences.widths[column], total: totalWidth, pixels: event.currentTarget.closest("table")?.getBoundingClientRect().width || 1 }; event.currentTarget.setPointerCapture?.(event.pointerId); }}
          onPointerMove={event => resize(column, event.clientX)} onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }} onLostPointerCapture={() => { drag.current = null; }} />
      </th>;
    })}</tr></thead>
    <tbody>{spots.map(spot => <tr key={spot.id} tabIndex={0} aria-selected={selectedSpotId === spot.id} aria-label={`Select ${spot.callsign} at ${formatSpotFrequency(spot.frequencyKHz * 1000)} MHz`}
      style={{ backgroundColor: classic || selectedSpotId === spot.id ? undefined : clusterRowTint(spot.dxccStatus, colors?.status) }}
      onClick={() => onSelect(spot)} onKeyDown={event => { if (menu.onKeyDown(event, spot)) return;
        if (event.key === "Enter" || event.key === " ") { event.preventDefault(); onSelect(spot); } }}>
      {preferences.columns.map(column => <td key={column} title={columnOverrides?.[column]?.value(spot) ?? spotText(column, spot, now)}
        onContextMenu={column === "callsign" && alertMenu ? event => menu.open(event, spot) : undefined}
        className={column === "frequency" ? "cyan mono" : column === "callsign" ? "live-cluster-call" : column === "callStatus" || column === "dxccStatus" ? "live-cluster-status-column" : ""}>{columnOverrides?.[column]?.value(spot) ?? cell(column, spot, now, colors, classic)}</td>)}
    </tr>)}</tbody>
  </table>{!spots.length && <p className="operator-cluster-empty">{emptyMessage}</p>}</div>{menu.overlay}</>;
}
