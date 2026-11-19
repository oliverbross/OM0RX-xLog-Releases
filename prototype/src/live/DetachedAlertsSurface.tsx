import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import { connectDxAlerts, saveDxAlerts, dxAlertError, dxRuleSummary, previewDxAlerts, reviewDxAlert, type DxAlertRule, type DxAlertsSnapshot } from "./dxAlerts";
import { AlertRuleGeography } from "./AlertRuleGeography";
import { useSettingsSaveToast } from "./SettingsSaveToast";
import { digitalPlatformLimitation } from "./digitalPlatform";
import "./DetachedReferenceSurface.css";
import "./AlertSettings.css";
import {invoke} from "@tauri-apps/api/core";
import {useSpotActivation} from "./useSpotActivation";
import {activationMessage} from "./spotActivationClient";
export function AlertSettings() {
  const digitalUnavailable = Boolean(digitalPlatformLimitation());
  const [ruleConflict,setRuleConflict]=useState(false);
  const [entities,setEntities]=useState<{adif:number;name:string}[]>([]);
  const [entityLoading, setEntityLoading] = useState(false), [entityError, setEntityError] = useState("");
  const entityRead = useRef(false);
  const loadEntities = async () => {
    if (entityRead.current) return;
    entityRead.current = true; setEntityLoading(true); setEntityError("");
    try {
      const value = await invoke<{adif:number;name:string}[]>("dx_alerts_entities");
      if (!Array.isArray(value)) throw new Error("DXCC entity directory unavailable.");
      setEntities(value);
    } catch { setEntityError("DXCC entity directory unavailable. Retry to select entities by name or ADIF ID."); }
    finally { entityRead.current = false; setEntityLoading(false); }
  };
  const [preview,setPreview] = useState<string>("");
  const [dx, setDx] = useState<DxAlertsSnapshot | null>(null), [draft, setDraft] = useState<DxAlertsSnapshot["configuration"] | null>(null);
  const [dxError, setDxError] = useState(""), [message, setMessage] = useState("");
  const saveToast = useSettingsSaveToast();
  const latestDx = useRef<DxAlertsSnapshot | null>(null);
  const receiveDx = (value: DxAlertsSnapshot) => {
    const previous = latestDx.current;
    if (previous?.epoch === value.epoch && previous.revision > value.revision) return;
    latestDx.current = value;
    setDx(value);
    setDraft(current => {
      if(current?.generation===value.configuration.generation && current.revision===value.configuration.revision)return current;
      const editable=(config:DxAlertsSnapshot["configuration"])=>{const {generation:_,revision:__,...fields}=config;return JSON.stringify(fields);};
      if(current&&previous&&editable(current)!==editable(previous.configuration)&&editable(current)!==editable(value.configuration)){
        setRuleConflict(true);return current;
      }
      return structuredClone(value.configuration);
    });
    setDxError("");
  };
  useEffect(() => connectDxAlerts(receiveDx, setDxError), []);
  const updateRule = (index: number, patch: Partial<DxAlertRule>) => setDraft(current => current ? ({ ...current, rules: current.rules.map((rule, position) => position === index ? { ...rule, ...patch } : rule) }) : current);
  const action = async (work: () => Promise<DxAlertsSnapshot>, success: string) => { try { setMessage(""); const value = await work(); if (latestDx.current?.epoch !== dx?.epoch) return; receiveDx(value); if (success.startsWith("Rules saved")) saveToast.show(success); else setMessage(success); } catch (cause) { setDxError(dxAlertError(cause, "DX alert action failed.")); } };
  return <div className="alert-settings"><h2>Alerts</h2><p>Save callsign targets in the Alerts window. Each band and mode remains needed until a matching contact is saved in your logbook.</p><button type="button" onClick={()=>void invoke("open_detached_map",{kind:"alerts"}).catch(cause => setDxError(dxAlertError(cause, "The Alerts window could not be opened.")))}>Open Alerts &amp; target list</button><section aria-label="DX opportunity alerts">

    {digitalUnavailable ? <p className="dx-alert-state">WSJT-X CQ alerts require macOS. Choose DX Cluster as the rule source on this computer.</p> : dx?.activitySource && <p className="dx-alert-state">WSJT-X CQ feed {dx.activitySource.available ? "listening" : "off or unavailable"} · receive only · FT8/FT4 · activity expires after 60 seconds. Call/DXCC novelty is unavailable for this source.</p>}{saveToast.toast}
    {dx?.storageError && <ErrorNotice><p className="dx-alert-error">{dx.storageError}</p></ErrorNotice>}{dxError && <ErrorNotice><p className="dx-alert-error" role="alert">{dxError}</p></ErrorNotice>}{message && <ErrorNotice><p className="dx-alert-message" role="status">{message}</p></ErrorNotice>}
    {dx?.delivery.error && <ErrorNotice><p className="dx-alert-error" role="alert">{dx.delivery.error}</p></ErrorNotice>}
    {!!dx?.delivery.suppressed && <p className="secondary">{dx.delivery.suppressed} native notifications suppressed by the {dx.delivery.maximumPerWindow}-per-{dx.delivery.windowSeconds}-second safety limit.</p>}
    {draft && dx && <details className="dx-alert-settings" onToggle={event => { if (event.target === event.currentTarget && event.currentTarget.open && !entities.length) void loadEntities(); }}><summary>Alert preferences and advanced rules</summary>
      {entityLoading && <ErrorNotice><p role="status">Loading DXCC entity directory…</p></ErrorNotice>}
      {entityError && <ErrorNotice><p role="alert">{entityError} <button type="button" onClick={() => void loadEntities()}>Retry entity directory</button></p></ErrorNotice>}
      <div className="dx-alert-global"><label><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled:event.currentTarget.checked })}/> Enable DX alerts</label>
        <label><input type="checkbox" checked={draft.nativePopup} onChange={event => setDraft({ ...draft, nativePopup:event.currentTarget.checked, notificationSound:event.currentTarget.checked ? draft.notificationSound : false })}/> Popup in main app and desktop</label>
        <label><input type="checkbox" checked={draft.notificationSound} disabled={!draft.nativePopup} onChange={event => setDraft({ ...draft, notificationSound:event.currentTarget.checked })}/> Notification sound</label>
        <label>Repeat guard (seconds)<input type="number" min="15" max="3600" value={draft.dedupSeconds} onChange={event => setDraft({ ...draft, dedupSeconds:Number(event.currentTarget.value) })}/></label>
        <label>Keep alerts (minutes)<input type="number" min="1" max="1440" value={draft.retentionMinutes} onChange={event => setDraft({ ...draft, retentionMinutes:Number(event.currentTarget.value) })}/></label></div>
      {draft.rules.map((rule,index) => rule.chaseModes?.length ? null : <div className="dx-alert-rule" key={rule.id}><div className="dx-alert-rule-head"><label><input type="checkbox" checked={rule.enabled} onChange={event => updateRule(index,{ enabled:event.currentTarget.checked })}/> On</label>
        <input type="text" aria-label={`Rule ${index + 1} name`} value={rule.name} maxLength={80} onChange={event => updateRule(index,{ name:event.currentTarget.value })}/>
        <button onClick={() => setDraft({ ...draft, rules:[...draft.rules,{ ...structuredClone(rule), id:crypto.randomUUID(), name:`${rule.name} copy` }] })} disabled={draft.rules.length >= dx.limits.maximumRules}>Clone</button>
        <button onClick={() => setDraft({ ...draft, rules:draft.rules.filter((_,position) => position !== index) })}>Remove</button></div>
        <div className="dx-alert-rule-fields"><label>Source<select value={(rule.sources ?? ["cluster"]).length === 2 ? "both" : rule.sources?.[0] ?? "cluster"} onChange={event => updateRule(index,{ sources:event.currentTarget.value === "both" ? ["cluster", "wsjtx"] : [event.currentTarget.value as "cluster" | "wsjtx"] })}><option value="cluster">DX Cluster</option><option value="wsjtx" disabled={digitalUnavailable}>WSJT-X CQ</option><option value="both" disabled={digitalUnavailable}>DX Cluster and WSJT-X CQ</option></select></label><label>Callsign pattern<input value={rule.callsignGlob} placeholder="* or OM0*" onChange={event => updateRule(index,{ callsignGlob:event.currentTarget.value.toUpperCase() })}/></label>
          <label>Comment contains<input value={rule.commentContains} maxLength={80} placeholder="e.g. POTA" onChange={event => updateRule(index,{ commentContains:event.currentTarget.value })}/></label>
          <label>Bands (comma separated)<input value={rule.bands.join(", ")} onChange={event => updateRule(index,{ bands:csv(event.currentTarget.value).map(value => value.toLowerCase()) })}/></label>
          <label>Modes: CW, PHONE, DATA<input value={rule.modes.join(", ")} onChange={event => updateRule(index,{ modes:csv(event.currentTarget.value).map(value => value.toLowerCase()) })}/></label>
          <label>DXCC status<input value={rule.dxccStatuses.join(", ")} onChange={event => updateRule(index,{ dxccStatuses:csv(event.currentTarget.value) })}/></label>
          <label>Call status<input value={rule.callStatuses.join(", ")} onChange={event => updateRule(index,{ callStatuses:csv(event.currentTarget.value) })}/></label>
          <label>Special tags<input value={rule.specials.join(", ")} onChange={event => updateRule(index,{ specials:csv(event.currentTarget.value).map(value => value.toUpperCase()) })}/></label></div>
        <details className="dx-alert-geography-options"><summary aria-label={`Rule ${index + 1} geography options`}>Geography · {[rule.dxEntities, rule.spotterEntities, rule.dxContinents, rule.spotterContinents, rule.dxCqZones, rule.dxItuZones].some(values => values?.length) ? "filters set" : "optional filters"}</summary>
          <AlertRuleGeography rule={rule} ruleNumber={index + 1} entities={entities} onChange={patch => updateRule(index, patch)} /></details></div>)}
      <p className="secondary">Rule summary: {draft.rules.filter(rule=>rule.enabled).map(dxRuleSummary).join("; ") || "No enabled rules"}</p>
      <button type="button" onClick={()=>void previewDxAlerts(draft.rules).then(value=>setPreview(value.matches.length ? value.matches.map(match=>`${match.callsign} ${match.frequencyKHz.toFixed(1)} kHz: ${match.ruleNames.join(", ")}`).join("; ") : "No retained reports match.")).catch(cause=>setDxError(String(cause)))}>Preview retained reports (read only)</button>
      {preview && <ErrorNotice><p role="status">{preview}</p></ErrorNotice>}
      <p className="secondary">Popups open in the main app and on the desktop. Viewing or dismissing a popup never tunes or transmits.</p>
      <p className="secondary">Each rule requires all its filled conditions. Comment contains matches literal text, ignoring case; leave it blank for any comment.</p>
      {ruleConflict&&<ErrorNotice><p role="alert">Rules changed in another window. Your attempted edits are retained. Revert to the current saved rules before making a new edit.</p></ErrorNotice>}
      <div className="dx-alert-actions"><button disabled={draft.rules.length >= dx.limits.maximumRules} onClick={() => setDraft({ ...draft, rules:[...draft.rules,newRule()] })}>Add rule</button><button onClick={() => {setDraft(structuredClone(dx.configuration));setRuleConflict(false);}}>Revert</button><button disabled={ruleConflict} className="primary" onClick={() => void action(() => saveDxAlerts(dx,{ enabled:draft.enabled,dedupSeconds:draft.dedupSeconds,retentionMinutes:draft.retentionMinutes,nativePopup:draft.nativePopup,notificationSound:draft.notificationSound,rules:draft.rules }), "Rules saved. Existing spots were baselined without alerts.")}>Save rules</button></div>
    </details>}
  </section></div>;
}
export { ChaseAlerts as DetachedAlertsSurface } from "./ChaseAlerts";

const csv = (value: string) => value.split(",").map(item => item.trim()).filter(Boolean);
const newRule = (): DxAlertRule => ({ id:crypto.randomUUID(), name:"New rule", enabled:true, sources:["cluster"], callsignGlob:"*", commentContains:"", bands:[], modes:[], callStatuses:[], dxccStatuses:[], specials:[] });
export function AlertReview({review,onClose,onError}:{review:Awaited<ReturnType<typeof reviewDxAlert>>;onClose:()=>void;onError:(message:string)=>void}) {
  const activation=useSpotActivation();
  const [requested,setRequested]=useState(false);
  const [alternative,setAlternative]=useState<NonNullable<typeof review.alternatives>[number]|null>(null);
  return <section aria-label="Review alert spot"><h3>{review.entry.callsign} · {review.entry.frequencyKHz.toFixed(1)} kHz</h3>
      <p>Matched at the recorded time: {review.entry.ruleNames.join(", ")} · {review.entry.callStatus} · {review.entry.dxccStatus}</p>
      <p>DXCC {review.entry.geography?.dxEntity??"unknown"} · continent {review.entry.geography?.dxContinent??"unknown"} · DX CQ {review.entry.geography?.dxCqZone??"unknown"} · DX ITU {review.entry.geography?.dxItuZone??"unknown"} · spotter DXCC {review.entry.geography?.spotterEntity??"unknown"} · spotter continent {review.entry.geography?.spotterContinent??"unknown"}</p>
      <p>{review.admissible ? "A matching live cluster report is available. Tune & load rechecks the active radio and shared editors." : review.reason || "Historical evidence only."}</p>
      <button type="button" disabled={requested || !(alternative || review.admissible) || !activation.canActivate()} onClick={()=>{setRequested(true);const observation=alternative||review.currentObservation||review.observation;if(observation)void activation.activate(observation).catch(cause=>onError(String(cause)));}}>Tune &amp; load{alternative?" current selection":""}</button>
      {!!review.alternatives?.length&&<div><p>Current ordinary cluster alternatives require an explicit selection:</p>{review.alternatives.map(spot=><button type="button" key={spot.id} onClick={()=>{setAlternative(spot);setRequested(false);}}>Review current {spot.callsign} · {spot.frequencyKHz.toFixed(1)} kHz</button>)}</div>}
      {alternative&&<ErrorNotice><p role="status">Current selection: {alternative.callsign} · {alternative.frequencyKHz.toFixed(1)} kHz. The historical alert above remains unchanged.</p></ErrorNotice>}
      <button type="button" onClick={()=>onClose()}>Close review</button>
      {requested&&(activation.error||activationMessage(activation.status))&&<ErrorNotice when={Boolean(activation.error) || undefined}><p role="status">{activation.error||activationMessage(activation.status)}</p></ErrorNotice>}
    </section>;
}
