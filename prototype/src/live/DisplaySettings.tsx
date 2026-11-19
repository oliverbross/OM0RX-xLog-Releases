import { ErrorNotice } from "./ErrorNotice";
import { useState } from "react";
import { useUiScale } from "./uiScale";
import { uiScales } from "./uiScaleController";
import { qsoDetailFieldKeys, qsoDetailFieldSections } from "./qsoFields";
import { useQSOFieldPreferences } from "./useQSOFieldPreferences";
import { propagationRefreshMinutes, usePropagationRefreshPreference } from "./propagationPreferences";
import { useClusterAppearancePreference } from "./clusterAppearance";
import { useLogbookAppearancePreference } from "./logbookAppearance";
import "./DisplaySettings.css";

export default function DisplaySettings() {
  const display = useUiScale();
  const qso = useQSOFieldPreferences();
  const propagation = usePropagationRefreshPreference();
  const clusterAppearance = useClusterAppearancePreference();
  const logbookAppearance = useLogbookAppearancePreference();
  const [changes, setChanges] = useState<{ scale?: number; appearance?: "modern" | "classic"; logbook?: "modern" | "classic"; minutes?: number; fields?: string[] }>({});
  const fields = changes.fields ?? qso.preference.fields;
  const selected = new Set(fields);
  const chooseFields = (fields: string[]) => setChanges(current => ({ ...current, fields }));
  const choose = (field: string, visible: boolean) => chooseFields(visible ? [...selected, field] : fields.filter(candidate => candidate !== field));
  const save = () => {
    if (changes.scale !== undefined) display.setScale(changes.scale);
    if (changes.appearance !== undefined) clusterAppearance.save(changes.appearance);
    if (changes.logbook !== undefined) logbookAppearance.save(changes.logbook);
    if (changes.minutes !== undefined) propagation.save(changes.minutes);
    if (changes.fields !== undefined) qso.save(changes.fields);
    setChanges({});
  };
  return <section className="display-settings" aria-label="Display settings">
    <h2>Display scale</h2><p>Enlarge the whole interface for a high-resolution display. 100% is the default; operating-system display scaling still applies. This changes layout only, not radio or rotor settings.</p>
    <label>Interface size<select aria-label="Interface display scale" value={changes.scale ?? display.requestedScale} disabled={!display.available} onChange={event => setChanges({ ...changes, scale: Number(event.target.value) })}>
      {uiScales.map(scale => <option key={scale} value={scale}>{Math.round(scale * 100)}%{scale === 1 ? " · Default" : ""}</option>)}
    </select></label>
    <div className="display-settings-actions"><button type="button" disabled={!display.available} onClick={() => { setChanges(({ scale, ...rest }) => rest); display.reset(); }}>Reset to 100%</button><span>⌘/Ctrl+0 works from any view.</span></div>
    <ErrorNotice><p role="status">{!display.available ? "Display controls are initializing." : display.pending ? `Applying ${Math.round(display.requestedScale * 100)}%…` : `Last applied in this window: ${Math.round(display.scale * 100)}%.`} Main, monitor and undocked views share the preference.</p></ErrorNotice>
    {display.error && <ErrorNotice><p role="alert" className="live-station-error">{display.error}</p></ErrorNotice>}
    <p>Band-map label size remains a separate preference. Higher interface scales need more screen space; Reset stays available by keyboard if controls move out of view.</p>
    <div className="display-qso-fields">
      <h2>Cluster appearance</h2>
      <label>Spot table style<select aria-label="Cluster appearance" value={changes.appearance ?? clusterAppearance.appearance}
        onChange={event => setChanges({ ...changes, appearance: event.target.value as "modern" | "classic" })}>
        <option value="modern">Modern · coloured</option><option value="classic">Classic · compact</option>
      </select></label>
      <p>Classic uses compact rows with coloured text. Modern uses coloured rows and badges. The DX Cluster window and Operator Desk share this preference. The Contest Cluster keeps its contest layout.</p>
      {clusterAppearance.error && <ErrorNotice><p role="alert" className="live-station-error">{clusterAppearance.error}</p></ErrorNotice>}
    </div>
    <div className="display-qso-fields">
      <h2>Logbook appearance</h2>
      <label>Contact table style<select aria-label="Logbook appearance" value={changes.logbook ?? logbookAppearance.appearance}
        onChange={event => setChanges({ ...changes, logbook: event.target.value as "modern" | "classic" })}>
        <option value="modern">Modern · current</option><option value="classic">Classic · compact like Cluster</option>
      </select></label>
      <p>Classic matches the compact Cluster rows and coloured text. All Logbook views share this preference; columns and widths are retained.</p>
      {logbookAppearance.error && <ErrorNotice><p role="alert" className="live-station-error">{logbookAppearance.error}</p></ErrorNotice>}
    </div>
    <div className="display-qso-fields">
      <h2>HF propagation</h2>
      <label>Automatic refresh<select aria-label="Propagation refresh interval" value={changes.minutes ?? propagation.minutes} onChange={event => setChanges({ ...changes, minutes: Number(event.target.value) })}>
        {propagationRefreshMinutes.map(minutes => <option key={minutes} value={minutes}>Every {minutes} minutes{minutes === 10 ? " · Default" : ""}</option>)}
      </select></label>
      <p>The Desk and Propagation window share the latest conditions and refresh on this interval while open.</p>
      {propagation.error && <ErrorNotice><p role="alert" className="live-station-error">{propagation.error}</p></ErrorNotice>}
    </div>
    <div className="display-qso-fields">
      <h2>QSO details</h2>
      <p>Choose which optional fields appear in the QSO details panel. Active-contact fields such as callsign, frequency, band, mode, reports and UTC always remain visible. Hiding a field never removes its saved value.</p>
      <div className="display-qso-field-actions"><button type="button" onClick={() => chooseFields(qsoDetailFieldKeys)}>Show all</button><button type="button" onClick={() => chooseFields([])}>Hide all optional fields</button><span>{selected.size} of {qsoDetailFieldKeys.length} shown</span></div>
      <div className="display-qso-field-groups">{qsoDetailFieldSections.map(section => <fieldset key={section.id}><legend>{section.label}</legend>{section.fields.map(field => <label key={field.key}><input type="checkbox" checked={selected.has(field.key)} onChange={event => choose(field.key, event.target.checked)} />{field.label}</label>)}</fieldset>)}</div>
      {qso.error && <ErrorNotice><p role="alert" className="live-station-error">{qso.error}</p></ErrorNotice>}
    </div>
    <button type="button" className="primary" disabled={!Object.keys(changes).length} onClick={save}>Save display settings</button>
  </section>;
}
