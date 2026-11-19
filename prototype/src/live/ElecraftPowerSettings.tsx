import { ErrorNotice } from "./ErrorNotice";
import { useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { bands, type Config, type PowerPolicy } from "./elecraftClient";
import "./AgentConfiguration.css";
import { powerPolicyStatus, subscribePowerPolicy, retryPowerPolicy } from "./elecraftPowerPolicy";

export const defaultPowerPolicy = (): PowerPolicy => ({ enabled: false, bands: Object.fromEntries(bands.map(band => [band,
  { pa_w: 25, barefoot_w: 100, digital_w: 10, pa_allowed: true, digital_pa_allowed: false }])) });

export default function ElecraftPowerSettings({ config, onChange, onSave, busy, loadAgent }: { config: Config; onChange: (config: Config) => void; onSave: () => void; busy: boolean; loadAgent?: () => Promise<Record<string, any>> }) {
  const policy = config.power_policy ?? defaultPowerPolicy(), [message, setMessage] = useState("");
  const status = useSyncExternalStore(subscribePowerPolicy, powerPolicyStatus);
  const change = (next: PowerPolicy) => onChange({ ...config, power_policy: next });
  const invalid = bands.some(band => !policy.bands[band] || [policy.bands[band].pa_w, policy.bands[band].barefoot_w, policy.bands[band].digital_w].some(w => !Number.isInteger(w) || w < 1 || w > 100));
  return <fieldset className="agent-config ec-power-policy" disabled={busy}><legend>Radio power by band and amplifier state</legend>
    <p>Radio drive in watts, not amplifier output. Direct FLEX applies this policy in receive when the band, mode or amplifier state changes.</p>
    <p>If your Station Agent already controls radio power, keep this local policy off and use its settings below.</p>
    <label><input type="checkbox" checked={policy.enabled} onChange={e => change({ ...policy, enabled: e.target.checked })}/>Automatically apply radio power policy</label>
    <div className="agent-table-scroll"><table className="agent-band-table"><thead><tr><th>Band</th><th>PA allowed</th><th>With PA · W</th><th>Without PA · W</th><th>Digital PA allowed</th><th>Digital · W</th></tr></thead><tbody>{bands.map(band => {
      const row = policy.bands[band];
      const update = (patch: Partial<typeof row>) => change({ ...policy, bands: { ...policy.bands, [band]: { ...row, ...patch } } });
      return <tr key={band}><th scope="row">{band}</th><td><input aria-label={`${band} PA allowed`} type="checkbox" checked={row.pa_allowed} onChange={e => update({ pa_allowed: e.target.checked })}/></td>
        {(["pa_w", "barefoot_w"] as const).map(key => <td key={key}><input aria-label={`${band} ${key === "pa_w" ? "With PA" : "Without PA"} watts`} type="number" min={1} max={100} step={1} value={row[key] || ""} onChange={e => update({ [key]: Number(e.target.value) })}/></td>)}
        <td><input aria-label={`${band} Digital PA allowed`} type="checkbox" checked={row.digital_pa_allowed} onChange={e => update({ digital_pa_allowed: e.target.checked })}/></td><td><input aria-label={`${band} Digital watts`} type="number" min={1} max={100} step={1} value={row.digital_w || ""} onChange={e => update({ digital_w: Number(e.target.value) })}/></td></tr>;
    })}</tbody></table></div>
    <p>Disallowed bands or digital modes place the PA in standby. Allowing the PA keeps your selected Operate/Standby state; it does not power on or key the amplifier. Digital watts apply in both states.</p>
    <div className="ec-actions"><button type="button" disabled={invalid} onClick={onSave}>Save power policy</button><button type="button" onClick={async () => {
      try {
        const agent = loadAgent ? await loadAgent() : await invoke<Record<string, any>>("elecraft_agent", { action: "load" });
        const p = agent.policy;
        const next = Object.fromEntries(bands.map(band => [band, { pa_w: p.amplifier_drive_w[band], barefoot_w: p.barefoot_drive_w[band], digital_w: p.digital_amplifier_drive_w[band], pa_allowed: p.amplifier_allowed_bands.includes(band), digital_pa_allowed: p.digital_kpa_policy === "operate" && p.digital_amplifier_allowed_bands.includes(band) }]));
        change({ enabled: false, bands: next }); setMessage("Agent values copied for review. Save to keep them; automatic application remains off.");
      } catch { setMessage("Agent power settings could not be read. Your current values are retained."); }
    }}>Copy power values from Station Agent</button></div>
    {policy.enabled && <div className="ec-actions"><ErrorNotice><output role="status">{status}</output></ErrorNotice><button type="button" onClick={retryPowerPolicy}>Retry power policy</button></div>}
    {invalid && <ErrorNotice><p role="alert">Enter whole-number drive levels from 1 to 100 W.</p></ErrorNotice>}{message && <ErrorNotice when={true} title="Message"><p role="status">{message}</p></ErrorNotice>}
  </fieldset>;
}
