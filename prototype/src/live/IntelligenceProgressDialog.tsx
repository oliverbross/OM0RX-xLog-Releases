import { ErrorNotice } from "./ErrorNotice";
import { useState } from "react";
import { AppDialog } from "./AppDialog";
import { BandModeProgressTable, type ProgressState } from "./BandModeProgressTable";
import { chaseModes } from "./chaseTargets";
type Cell = { band: string; mode: string; worked?: boolean; confirmed?: boolean };
const modeKey = (mode: string) => ({ USB: "SSB", LSB: "SSB", PHONE: "SSB", CWR: "CW", RTTYR: "RTTY" }[mode.trim().toUpperCase()] ?? mode.trim().toUpperCase());
export default function IntelligenceProgressDialog({ call, adif, country, bands, callCells, dxccCells, error, onClose }: {
  call: string; adif: number | null; country: string; bands: string[]; callCells?: Cell[]; dxccCells?: Cell[]; error?: string; onClose: () => void;
}) {
  const [scope, setScope] = useState<"call" | "dxcc">("call");
  const cells = scope === "call" ? callCells : dxccCells;
  const modes = [...chaseModes, ...Array.from(new Set([...(callCells ?? []), ...(dxccCells ?? [])].map(cell => modeKey(cell.mode ?? "")).filter(mode => mode && !chaseModes.includes(mode)))).sort()];
  const state = (band: string, mode: string): ProgressState => {
    if (!cells) return "unknown";
    const matches = cells.filter(cell => cell.band === band && modeKey(cell.mode ?? "") === mode);
    return matches.some(cell => cell.confirmed) ? "confirmed" : matches.some(cell => cell.worked) ? "worked" : "needed";
  };
  const states = bands.flatMap(band => modes.map(mode => state(band, mode)));
  const worked = states.filter(value => value === "worked" || value === "confirmed").length, confirmed = states.filter(value => value === "confirmed").length;
  return <AppDialog title="Logbook progress" className="chase-list-dialog intelligence-progress" onClose={onClose}>
    <nav className="intelligence-progress-scopes" aria-label="Progress scope">
      <button type="button" aria-pressed={scope === "call"} onClick={() => setScope("call")}>Callsign</button>
      <button type="button" aria-pressed={scope === "dxcc"} disabled={!adif} onClick={() => setScope("dxcc")}>DXCC</button>
    </nav>
    <header><strong>{scope === "call" ? `Callsign · ${call}` : `DXCC · ${country || "Entity"} (#${adif})`}</strong>
      <span>{cells ? `${states.length - worked} needed · ${worked} worked · ${confirmed} confirmed` : error ? "Local history unavailable" : "Checking logbook…"}</span></header>
    <BandModeProgressTable label={`${scope === "call" ? `Callsign ${call}` : `DXCC ${adif}`} band and mode progress`} bands={bands} modes={modes} state={state} confirmations/>
    {!cells && <ErrorNotice when={Boolean(error) || undefined}><p role="status">{error || "Waiting for saved logbook history."}</p></ErrorNotice>}
    {!adif && <p className="intelligence-progress-note">DXCC progress is available once the callsign’s entity is resolved.</p>}
  </AppDialog>;
}
