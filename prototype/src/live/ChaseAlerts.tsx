import { ErrorNotice } from "./ErrorNotice";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AppDialog } from "./AppDialog";
import { connectDxAlerts, saveDxAlerts, dxAlertError, type DxAlertEntry, type DxAlertRule, type DxAlertsSnapshot } from "./dxAlerts";
import { ChaseAlertsTable } from "./ChaseAlertsTable";
import { formatRadioFrequency } from "./radioFrequency";
import { ChaseChoices as Choices } from "./ChaseTargetDialog";
import { chaseBands, chaseModes, chaseRule, saveChaseTarget } from "./chaseTargets";
export { chaseBands, chaseModes, chaseRule, targetCells } from "./chaseTargets";
import "./ChaseAlerts.css";
const ChaseTargetList = lazy(() => import("./ChaseTargetList").then(module => ({ default: module.ChaseTargetList })));

export function ChaseAlerts() {
  const [snapshot,setSnapshot]=useState<DxAlertsSnapshot|null>(null),[error,setError]=useState("");
  const [call,setCall]=useState(""),[bands,setBands]=useState<string[]>([]),[modes,setModes]=useState<string[]>([]);
  const [editing,setEditing]=useState<string|null>(null),[list,setList]=useState(false),[busy,setBusy]=useState(false),[message,setMessage]=useState("");
  useEffect(()=>connectDxAlerts(setSnapshot,setError),[]);
  const targets=snapshot?.configuration.rules.filter(rule=>rule.chaseModes?.length)??[];
  const save=async(rules:DxAlertRule[],added=false)=>{
    if(!snapshot||busy)return;setBusy(true);setError("");
    try{const {generation:_,revision:__,...config}=snapshot.configuration;
      const next=await saveDxAlerts(snapshot,{...config,rules:added&&!config.enabled?rules.map(r=>r.chaseModes?.length?r:{...r,enabled:false}):rules,...(added?{enabled:true,nativePopup:true}:{})});setSnapshot(next);setMessage("Alerts saved");return true;
    }catch(e){setError(String(e));return false;}finally{setBusy(false);}
  };
  const submit = async (form: HTMLFormElement) => {
    if (!snapshot || busy) return;
    setBusy(true); setError("");
    try {
      const rule = chaseRule(String(new FormData(form).get("callsign") ?? ""), bands, modes, editing ?? undefined);
      setSnapshot(await saveChaseTarget(snapshot, rule)); setMessage("Alerts saved");
      setEditing(null); setCall(""); setBands([]); setModes([]); form.reset();
    } catch (cause) { setError(dxAlertError(cause, "Could not save the alert. Your choices are retained.")); }
    finally { setBusy(false); }
  };
  return <div className="app live chase-alerts"><main aria-label="Alerts"><header><div><h1>Alerts</h1><span className="secondary">{snapshot?.source.available?"Cluster connected":"Waiting for cluster"} · {targets.length} saved targets</span></div><button type="button" onClick={()=>setList(true)}>Alerts list ({targets.length})</button></header>
    <form className="chase-entry" onSubmit={event=>{event.preventDefault();void submit(event.currentTarget);}}><label className="chase-call-field">Callsign<input key={editing ?? "new"} name="callsign" aria-label="Alert callsign" defaultValue={call} maxLength={24} autoComplete="off" spellCheck={false} disabled={busy} onChange={e=>setCall(e.target.value.toUpperCase())}/></label>
      <Choices label="Bands" values={chaseBands} selected={bands} onChange={setBands} disabled={busy}/><Choices label="Modes" values={chaseModes} selected={modes} onChange={setModes} disabled={busy}/>
      <button className="primary" disabled={busy||!snapshot} type="submit">{editing?"Save changes":"Save alert"}</button>{editing&&<button type="button" onClick={()=>{setEditing(null);setCall("");setBands([]);setModes([]);}}>Cancel edit</button>}
    </form>
    {message&&<ErrorNotice><p role="status">{message}</p></ErrorNotice>}{error&&<ErrorNotice><p role="alert">{error}</p></ErrorNotice>}{snapshot?.storageError&&<ErrorNotice><p role="alert">{snapshot.storageError}</p></ErrorNotice>}
    {!snapshot?.configuration.enabled&&<p>Alerts are paused in Settings → Application → Alerts.</p>}
    {!!targets.length&&!snapshot?.progressReady&&<ErrorNotice><p role="status">Checking saved contacts for completed targets…</p></ErrorNotice>}
    <ChaseAlertsTable snapshot={snapshot} />
    {list&&<AppDialog title="Alerts list" className="chase-list-dialog" onClose={()=>setList(false)}>
      <Suspense fallback={<ErrorNotice><p role="status">Opening saved targets…</p></ErrorNotice>}><ChaseTargetList targets={targets} completed={snapshot?.completed ?? []} ready={snapshot?.progressReady === true} busy={busy}
        onEdit={rule=>{setEditing(rule.id);setCall(rule.callsignGlob);setBands(rule.bands);setModes(rule.chaseModes??[]);setList(false);}}
        onRemove={rule=>snapshot&&void save(snapshot.configuration.rules.filter(r=>r.id!==rule.id))}/></Suspense>
      {error&&<ErrorNotice><p role="alert">{error}</p></ErrorNotice>}
    </AppDialog>}
  </main></div>;
}

/** The native owner applies repetition and rate limits; only main renders popups. */
export function MainAlertPopup(){
  const [entries,setEntries]=useState<DxAlertEntry[]>([]),[error,setError]=useState("");const seen=useRef(new Set<string>());
  useEffect(()=>{let active=true,stop:(()=>void)|undefined;void listen<DxAlertEntry>("xlog:dx-alert-popup",({payload})=>{if(!active||seen.current.has(payload.id))return;seen.current.add(payload.id);if(seen.current.size>500)seen.current.delete(seen.current.values().next().value!);setEntries(old=>[...old,payload].slice(-10));}).then(dispose=>{if(active)stop=dispose;else dispose();}).catch(()=>{if(active)setError("Alert popups are unavailable. Open Alerts to review matching stations.");});return()=>{active=false;stop?.();};},[]);
  useEffect(()=>connectDxAlerts(snapshot=>setEntries(old=>old.filter(e=>snapshot.entries.some(n=>n.id===e.id))),()=>{}),[]);
  const entry=entries[0];if(!entry)return error?<ErrorNotice when={Boolean(error) || undefined}><p role="status">{error} <button onClick={()=>setError("")}>Dismiss</button></p></ErrorNotice>:null;
  return <AppDialog title={`Needed station · ${entry.callsign}`} onClose={()=>setEntries(old=>old.slice(1))}><p className="chase-popup-call">{entry.callsign}</p><p>{entry.band} · {entry.mode} · <b className="cyan">{formatRadioFrequency(entry.frequencyKHz*1000)} MHz</b></p><p>{entry.country} {entry.comment}</p><div className="dialog-actions"><button onClick={()=>setEntries(old=>old.slice(1))}>Dismiss</button><button className="primary" onClick={()=>void invoke("open_detached_map",{kind:"alerts"}).then(()=>setEntries(old=>old.slice(1))).catch(e=>setError(String(e)))}>View alerts</button></div>{error&&<ErrorNotice><p role="alert">{error}</p></ErrorNotice>}</AppDialog>;
}

