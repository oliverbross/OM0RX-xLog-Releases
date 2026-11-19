import { useState } from "react";
import { ArrowLeft } from "lucide-react";
import { BandModeProgressTable } from "./BandModeProgressTable";
import type { DxAlertRule } from "./dxAlerts";
import { chaseBands, chaseModes, targetCells } from "./chaseTargets";

const pageSize = 6;
export function ChaseTargetList({ targets, completed, ready, busy, onEdit, onRemove }: {
  targets: DxAlertRule[]; completed: string[]; ready: boolean; busy: boolean;
  onEdit: (rule: DxAlertRule) => void; onRemove: (rule: DxAlertRule) => void;
}) {
  const [query, setQuery] = useState(""), [page, setPage] = useState(0), [selected, setSelected] = useState<string | null>(null);
  const done = new Set(completed), detail = targets.find(rule => rule.id === selected);
  const filtered = targets.filter(rule => rule.callsignGlob.includes(query.trim().toUpperCase()));
  const pages = Math.max(1, Math.ceil(filtered.length / pageSize)), current = Math.min(page, pages - 1);
  const progress = (rule: DxAlertRule) => {
    const cells = targetCells(rule), worked = cells.filter(cell => done.has(cell.key)).length;
    return ready ? `${cells.length - worked} needed · ${worked} worked` : "Checking logbook…";
  };
  const summary = (rule: DxAlertRule) => `${rule.bands.length === chaseBands.length ? "All bands" : rule.bands.length <= 3 ? chaseBands.filter(band => rule.bands.includes(band)).join(", ") : `${rule.bands.length} bands`} · ${(rule.chaseModes?.length ?? 0) === chaseModes.length ? "All modes" : (rule.chaseModes?.length ?? 0) <= 3 ? chaseModes.filter(mode => rule.chaseModes?.includes(mode)).join(", ") : `${rule.chaseModes!.length} modes`}`;
  const actions = (rule: DxAlertRule) => <><button type="button" disabled={busy} onClick={() => onEdit(rule)}>Edit</button><button type="button" disabled={busy} onClick={() => onRemove(rule)}>Remove</button></>;
  return <section className="chase-target-list" aria-label="Saved callsign targets">
    {detail ? <>
      <button type="button" className="chase-target-back" onClick={() => setSelected(null)}><ArrowLeft size={14} aria-hidden="true"/>All targets</button>
      <article className="chase-target-detail" aria-label={`${detail.callsignGlob} saved target`}>
        <header><div><strong>{detail.callsignGlob}</strong><span>{progress(detail)}</span></div><div className="chase-target-actions">{actions(detail)}</div></header>
        <BandModeProgressTable label={`${detail.callsignGlob} band and mode progress`} bands={chaseBands.filter(band => detail.bands.includes(band))}
          modes={chaseModes.filter(mode => detail.chaseModes?.includes(mode))} state={(band, mode) => !ready ? "unknown" : done.has(`${detail.id}|${band}|${mode}`) ? "worked" : "needed"}/>
      </article>
    </> : <>
      <div className="chase-list-toolbar"><span>{targets.length} saved {targets.length === 1 ? "target" : "targets"}</span>{targets.length > pageSize && <input aria-label="Filter saved callsigns" placeholder="Find callsign" value={query} onChange={event => { setQuery(event.target.value); setPage(0); }}/>}</div>
      <div className="chase-target-rows">{filtered.slice(current * pageSize, (current + 1) * pageSize).map(rule => <article className="chase-target-row" key={rule.id} aria-label={`${rule.callsignGlob} saved target`}>
        <div className="chase-target-summary"><strong title={rule.callsignGlob}>{rule.callsignGlob}</strong><span>{summary(rule)}</span><span className="chase-target-progress">{progress(rule)}</span></div>
        <div className="chase-target-actions"><button type="button" aria-label={`Show progress for ${rule.callsignGlob}`} onClick={() => setSelected(rule.id)}>Progress</button>{actions(rule)}</div>
      </article>)}</div>
      {!filtered.length && <p className="chase-list-empty">{targets.length ? "No matching callsigns." : "No callsign targets saved yet."}</p>}
      {filtered.length > pageSize && <nav className="chase-list-pages" aria-label="Saved target pages"><button type="button" disabled={!current} onClick={() => setPage(current - 1)}>Previous</button><span>{current * pageSize + 1}–{Math.min((current + 1) * pageSize, filtered.length)} of {filtered.length}</span><button type="button" disabled={current + 1 >= pages} onClick={() => setPage(current + 1)}>Next</button></nav>}
    </>}
    <p className="chase-list-note">Worked combinations stop alerting automatically.</p>
  </section>;
}
