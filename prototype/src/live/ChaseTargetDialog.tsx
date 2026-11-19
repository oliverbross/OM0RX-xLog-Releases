import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import { AppDialog } from "./AppDialog";
import { connectDxAlerts, dxAlertError, type DxAlertsSnapshot } from "./dxAlerts";
import { chaseBands, chaseMode, chaseModes, chaseRule, saveChaseTarget } from "./chaseTargets";
import "./ChaseAlerts.css";

export function ChaseChoices({ label, values, selected, onChange, disabled = false }: {
  label: string; values: string[]; selected: string[]; onChange: (values: string[]) => void; disabled?: boolean;
}) {
  return <fieldset className="chase-choices" disabled={disabled}><legend>{label}</legend><div>{values.map(value => <button type="button" key={value} aria-pressed={selected.includes(value)}
    onClick={() => onChange(selected.includes(value) ? selected.filter(item => item !== value) : [...selected, value])}>{value}</button>)}</div></fieldset>;
}

export function ChaseTargetDialog({ callsign, band, mode, onClose }: { callsign: string; band?: string; mode?: string; onClose: () => void }) {
  const [bands, setBands] = useState(chaseBands.includes(band ?? "") ? [band!] : []);
  const [modes, setModes] = useState(chaseModes.includes(chaseMode(mode ?? "")) ? [chaseMode(mode!)] : []);
  const [snapshot, setSnapshot] = useState<DxAlertsSnapshot | null>(null), [error, setError] = useState("");
  const [busy, setBusy] = useState(false), saving = useRef(false);
  const generation = useRef<string | null>(null);
  useEffect(() => connectDxAlerts(value => {
    if (generation.current && generation.current !== value.configuration.generation) {
      setSnapshot(null); setError("The saved Alerts configuration was replaced. Close and reopen this dialog before saving."); return;
    }
    generation.current = value.configuration.generation; setSnapshot(value);
  }, setError), []);
  return <AppDialog title="Add to Alerts" closeDisabled={busy} onClose={onClose}>
    <form className="chase-target-dialog" onSubmit={async event => {
      event.preventDefault();
      if (!snapshot || saving.current) return;
      const value = new FormData(event.currentTarget).get("callsign");
      saving.current = true; setBusy(true); setError("");
      try { await saveChaseTarget(snapshot, chaseRule(String(value ?? ""), bands, modes)); onClose(); }
      catch (cause) { setError(dxAlertError(cause, "Could not save the alert. Your choices are retained; try again.")); }
      finally { saving.current = false; setBusy(false); }
    }}>
      <label className="chase-call-field">Callsign<input data-dialog-autofocus name="callsign" aria-label="Alert callsign" defaultValue={callsign.toUpperCase()} maxLength={24} autoComplete="off" spellCheck={false}
        disabled={busy} /></label>
      <p>Choose the band and mode combinations to chase. Each stays needed until a matching QSO is saved.</p>
      <ChaseChoices label="Bands" values={chaseBands} selected={bands} onChange={setBands} disabled={busy} />
      <ChaseChoices label="Modes" values={chaseModes} selected={modes} onChange={setModes} disabled={busy} />
      {error && <ErrorNotice><p role="alert" className="chase-form-error">{error}</p></ErrorNotice>}
      <footer className="dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button>
        <button className="primary" type="submit" disabled={!snapshot || busy}>{busy ? "Saving…" : "Save alert"}</button></footer>
    </form>
  </AppDialog>;
}
