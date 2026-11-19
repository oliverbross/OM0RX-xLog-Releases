import { ErrorNotice } from "./ErrorNotice";
import { useState } from "react";
import { AppDialog } from "./AppDialog";
import { decimalMHz, formatRadioFrequency } from "./radioFrequency";
import type { StationProvider } from "../worker";
type Data = Record<string, any>;
// Standard SmartSDR slice modes. Advertised waveform modes extend this list.
// https://github.com/flexradio/smartsdr-api-docs/wiki/SmartSDR-Supported-Modes
export const flexModes = ["CW","USB","LSB","AM","SAM","FM","NFM","DFM","DIGU","DIGL","RTTY"];
const bands: [string,number][] = [["160m",1.82],["80m",3.55],["60m",5.354],["40m",7.03],["30m",10.12],["20m",14.05],["17m",18.08],["15m",21.05],["12m",24.91],["10m",28.05],["6m",50.1],["4m",70.1],["2m",144.05],["70cm",432.05]];
export function radioChoices(snapshot: Data|null, provider: StationProvider) {
  const advertised = snapshot?.capabilities?.supported_modes ?? snapshot?.tci?.supported_modulations ?? snapshot?.station?.supported_modes;
  const modes = [...new Set<string>([...(provider === "direct_flex_lan" ? flexModes : []),...(Array.isArray(advertised)?advertised.filter((v:unknown)=>typeof v==="string"&&/^[A-Z0-9]{1,16}$/i.test(v as string)).map((v:string)=>v.toUpperCase()):[]),...(snapshot?.radio?.mode?[snapshot.radio.mode]:[])])];
  const supported = snapshot?.capabilities?.supported_bands;
  const range = snapshot?.tci?.frequency_range_hz ?? snapshot?.capabilities?.frequency_range_hz;
  return {modes,bands:bands.filter(([band,mhz])=>Array.isArray(supported)?supported.includes(band):Array.isArray(range)&&range.length===2?mhz*1e6>=range[0]&&mhz*1e6<=range[1]:provider==="direct_flex_lan"?mhz<=54:false)};
}
export function StationRadioDialog({kind,snapshot,provider,onRequest,onClose}:{kind:"frequency"|"mode";snapshot:Data|null;provider:StationProvider;onRequest:(method:string,params?:Data)=>Promise<Data>;onClose:()=>void}) {
  const [original] = useState(()=>({provider,vfo:snapshot?.radio?.receive_vfo,generation:snapshot?.generation}));
  const [input,setInput]=useState(()=>snapshot?.radio?.frequency_hz?formatRadioFrequency(snapshot.radio.frequency_hz):"");
  const [mode,setMode]=useState(()=>snapshot?.radio?.mode??""),[busy,setBusy]=useState(false),[error,setError]=useState("");
  const choices=radioChoices(snapshot,provider),radio=snapshot?.radio;
  const hz=Math.round(Number(decimalMHz(input))*1e6);
  const valid=/^\d+(?:\.\d{1,6})?$/.test(decimalMHz(input))&&Number.isSafeInteger(hz)&&hz>=100000&&hz<=1000000000;
  const apply=async()=>{if(busy)return;setError("");
    if(provider!==original.provider||snapshot?.generation!==original.generation||radio?.receive_vfo!==original.vfo){setError("Radio selection changed. Close this dialog and select the current radio again.");return;}
    if(!radio?.connected||!radio.observed||radio.transmitting!==false){setError("Wait for the connected radio to return to receive.");return;}
    setBusy(true);try{
      const params=provider==="direct_flex_lan"||provider==="direct_agent"
        ? kind==="mode"?{control:"set_mode",mode}:{control:radio.receive_vfo==="B"?"set_frequency_b":"set_frequency_a",frequency_hz:hz}
        : {frequency_hz:kind==="frequency"?hz:radio.frequency_hz,mode:kind==="mode"?mode:radio.mode};
      const result=await onRequest(provider==="direct_flex_lan"||provider==="direct_agent"?"station_radio_control":"station_tune",{provider,...params});
      if(result.confirmed!==true)throw Error("Change sent; radio confirmation is still pending. Check the radio before another change.");
      onClose();
    }catch(e){setError(e instanceof Error?e.message:String(e));}finally{setBusy(false);}
  };
  return <AppDialog title={kind==="mode"?"Radio mode":"Frequency & bands"} onClose={onClose}>
    <small>Receive {radio?.receive_vfo?`VFO ${radio.receive_vfo}`:"radio"} · {radio?.frequency_hz?formatRadioFrequency(radio.frequency_hz):"—"} MHz · {radio?.mode??"—"}</small>
    {kind==="mode"?<><p>Choose a mode</p><div className="dialog-choices">{choices.modes.map(value=><button type="button" key={value} aria-pressed={mode===value} onClick={()=>setMode(value)}>{value}</button>)}</div>{choices.modes.length<=1&&<p>This radio has not reported its other supported modes.</p>}</>:<><label style={{marginTop:18}}>Frequency in MHz<input data-dialog-autofocus aria-label="Radio frequency MHz" inputMode="decimal" value={input} onChange={e=>setInput(e.target.value)} onKeyDown={e=>{if(e.key==="Enter"&&valid){e.preventDefault();void apply();}}} style={{fontSize:30,fontVariantNumeric:"tabular-nums",color:"#4ee2f0",marginTop:6}}/></label><small>For example 7.029.13 or 7.029130</small>{!!choices.bands.length&&<><p>Choose a band</p><div className="dialog-choices">{choices.bands.map(([band,mhz])=><button type="button" key={band} onClick={()=>setInput(formatRadioFrequency(mhz*1e6))}>{band}</button>)}</div><small>Band buttons fill a receive frequency. Apply when ready.</small></>}</>}
    {error&&<ErrorNotice><p role="alert">{error}</p></ErrorNotice>}<div className="dialog-actions"><button type="button" onClick={onClose}>Cancel</button><button type="button" className="primary" disabled={busy||(kind==="frequency"?!valid:!mode)} onClick={()=>void apply()}>{busy?"Applying…":kind==="mode"?"Apply mode":"Apply frequency"}</button></div>
  </AppDialog>;
}
