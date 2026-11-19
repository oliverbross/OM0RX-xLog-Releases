import type { LiveClusterSpot } from "./ClusterPanel";
import { compactClusterColumns, defaultWorkspacePreferences, readWorkspacePreferences, type CompactColumn, type WorkspacePreferences } from "./operatorWorkspacePreferences";
export const specialProgrammes = [["pota", "POTA"], ["sota", "SOTA"], ["iota", "IOTA"], ["wwff", "WWFF"], ["castle", "Castles"], ["other", "Other special"]] as const;
export type SpecialFilter = "all" | "new" | "worked";
export type SpecialPreferences = WorkspacePreferences & { programmes: string[]; progress: SpecialFilter };
export type SpecialEvidence = { id: string; callsign: string; frequencyKHz: number; band: string; mode: string; comment: string;
  programmes: string[]; references: { kind: string; reference: string; status: "new" | "worked" | "unknown" }[] };
export type SpecialSnapshot = { spots: SpecialEvidence[]; history_available: boolean };
export const specialKey = "xlog.integration.special-window.v1";
export function specialPreferences(raw: string | null): SpecialPreferences {
  const defaults: SpecialPreferences = { ...defaultWorkspacePreferences(), columns: ["time", "band", "frequency", "callsign", "mode", "special", "workedBefore"],
    programmes: specialProgrammes.map(([key]) => key), progress: "all" };
  defaults.widths.special = 220; defaults.widths.workedBefore = 150;
  if (!raw || raw.length > 4096) return defaults;
  try {
    const value = JSON.parse(raw), workspace = readWorkspacePreferences(raw);
    if (value.version !== 1) return defaults;
    const columns: CompactColumn[] = Array.isArray(value.columns) ? [...new Set<CompactColumn>(value.columns.filter((key: string) => compactClusterColumns.some(([id]) => id === key)))] : defaults.columns;
    if (!columns.includes("callsign")) columns.unshift("callsign");
    return { ...workspace, columns, programmes: Array.isArray(value.programmes) ? specialProgrammes.map(([id]) => id).filter(id => value.programmes.includes(id)) : defaults.programmes,
      progress: ["all", "new", "worked"].includes(value.progress) ? value.progress : "all" };
  } catch { return defaults; }
}
export const sameSpecialReport = (spot: LiveClusterSpot, row: SpecialEvidence) =>
  spot.id === row.id && spot.callsign === row.callsign && spot.frequencyKHz === row.frequencyKHz && spot.band === row.band && spot.mode === row.mode && spot.comment === row.comment;
/** Keep one current native token per activation reported by multiple clusters. */
export function distinctSpecialReports(spots: LiveClusterSpot[], evidence: SpecialEvidence[]) {
  const reports = new Map<string, { spot: LiveClusterSpot; row: SpecialEvidence }>();
  for (const spot of spots) {
    const row = evidence.find(value => sameSpecialReport(spot, value));
    if (!row) continue;
    const key = JSON.stringify([spot.callsign, spot.frequencyKHz, spot.band, spot.mode, [...row.programmes].sort(),
      row.references.map(ref => [ref.kind, ref.reference]).sort(), [...spot.specialTags].sort()]);
    const previous = reports.get(key);
    if (!previous || spot.receivedAt > previous.spot.receivedAt) reports.set(key, { spot, row });
  }
  return [...reports.values()];
}
export function specialProgress(spot: LiveClusterSpot, row: SpecialEvidence, programmes: string[], ready: boolean) {
  const references = row.references.filter(ref => programmes.includes(ref.kind));
  const newRefs = ready ? references.filter(ref => ref.status === "new") : [];
  const newCall = ready && spot.callStatus === "New Call";
  const workedCall = ready && (["New Band", "New Mode", "New Band+Mode", "Worked", "Confirmed"].includes(spot.callStatus) || Number.isFinite(spot.workedBefore));
  const isNew = newCall || newRefs.length > 0;
  const kindLabel: Record<string, string> = { pota: "park", sota: "summit", iota: "island", wwff: "WWFF ref", castle: "castle" };
  const progress = !ready ? "History unavailable" : newRefs.length > 1 ? `${newRefs.length} new references` : newRefs.length ? `New ${kindLabel[newRefs[0].kind] ?? "reference"}` : newCall ? "New callsign" : references.length && references.every(ref => ref.status === "worked") ? "Reference worked" : workedCall ? "Call worked · ref unknown" : "Reference unknown";
  return { isNew, worked: !isNew && (workedCall || ready && references.some(ref => ref.status === "worked")), progress,
    reference: references.length ? references.map(ref => `${ref.kind.toUpperCase()} ${ref.reference}`).join(" · ") : row.programmes.filter(kind => programmes.includes(kind)).map(kind => kind === "other" ? "Other special" : `${kind.toUpperCase()} · ref unknown`).join(" · ") };
}
