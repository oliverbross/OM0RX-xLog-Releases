import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import { workerRequest } from "../worker";
import type { RadioID } from "./stationSession";
import { formatRadioFrequency } from "./radioFrequency";

type Observation = {available:boolean; radio?:{observed:boolean;frequency_hz:number;mode:string};station?:{kpa_operate:boolean|null;tx_power_watts:number|null;swr_is_live:boolean;band:string}};
export function XCatCompanion({radioId}:{radioId:RadioID}) {
 const [port,setPort]=useState(()=>localStorage.getItem("xlog.xcat.port")??"14532"),[active,setActive]=useState<number|null>(null);
 const [observed,setObserved]=useState<Observation|null>(null),[message,setMessage]=useState(""),[attempt,setAttempt]=useState(0);
 const valid=/^\d+$/.test(port)&&Number(port)>0&&Number(port)<=65535;
 const revision=useRef(0);
 useEffect(()=>{
   if(active===null)return;
   const current=++revision.current;let pending=false;
   async function read(){if(pending)return;pending=true;
     try{const frame=await workerRequest("station_snapshot",{radio_id:radioId,provider:"xcat_loopback",host:"127.0.0.1",port:active});
       if(current===revision.current){setObserved(frame.result as unknown as Observation);setMessage("");}}
     catch(e){if(current===revision.current){setObserved(null);setMessage(e instanceof Error?e.message:"The xCat listener did not respond.");}}
     finally{pending=false;}
   }
   void read();const tick=setInterval(()=>void read(),2000);
   return()=>{revision.current++;clearInterval(tick);};
 },[active,radioId,attempt]);
 return <details className="live-station-card"><summary>Standalone xCat · loopback</summary><p>Observe an existing companion on this PC. Other loggers can also use the companion’s CAT and RigCtl ports.</p>
 <label>RigCtl port <input aria-label="Standalone xCat port" value={port} onChange={e=>setPort(e.target.value)} inputMode="numeric"/></label>
 <button disabled={!valid} onClick={()=>{localStorage.setItem("xlog.xcat.port",port);setMessage("Companion port saved.");}}>Save port</button>
 <button disabled={!valid} onClick={()=>{setObserved(null);setActive(Number(port));setAttempt(a=>a+1);}}> {active===null?"Connect":"Reconnect"}</button>
 <button disabled={active===null} onClick={()=>{setActive(null);setObserved(null);setMessage("Disconnected.");}}>Disconnect</button>
 {observed&&<p>{observed.radio?.observed?formatRadioFrequency(observed.radio.frequency_hz)+" MHz · "+observed.radio.mode:"Radio unavailable"} · {observed.station?.kpa_operate==null?"Amplifier unknown":observed.station.kpa_operate?"KPA Operate":"KPA Standby"}{observed.station?.tx_power_watts!=null?" · "+observed.station.tx_power_watts+" W":""}</p>}<ErrorNotice when={true} title="Message"><output>{message}</output></ErrorNotice></details>;
}
