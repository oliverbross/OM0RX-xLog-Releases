import { invoke, isTauri } from "@tauri-apps/api/core";
import { useRef, useState } from "react";

export async function saveTextFile(filename:string,content:string,type="application/json",
    command:"contest_analytics_export"|"logbook_export_file"="contest_analytics_export"):Promise<string>{
  if(isTauri()){
    const result=await invoke<{saved:boolean;cancelled:boolean}>(command,{filename,content});
    if(result?.cancelled===true&&result.saved===false)return "Export cancelled.";
    if(result?.saved===true&&result.cancelled===false)return "Export saved.";
    throw new Error("The export did not complete. Please try again.");
  }
  const url=URL.createObjectURL(new Blob([content],{type})),anchor=document.createElement("a");
  try{anchor.href=url;anchor.download=filename;document.body.appendChild(anchor);anchor.click();}
  finally{anchor.remove();setTimeout(()=>URL.revokeObjectURL(url),command==="logbook_export_file"?60_000:1000);}
  return "Download requested.";
}

export const saveAnalyticsFile=saveTextFile;

export function saveLogbookFile(content:string,format:"adi"|"adx"):Promise<string>{
  return saveTextFile(logbookExportFilename(format),content,
    format==="adx"?"application/xml;charset=utf-8":"text/plain;charset=utf-8","logbook_export_file");
}

/** Where a streamed Logbook export is written: a native file stream chosen up
 *  front (Tauri), or an in-memory download (browser preview). */
export interface LogbookExportSink {
  append(text:string):Promise<void>;
  finish(header:string,footer:string):Promise<string>;
  abort():Promise<void>;
}

export function logbookExportFilename(format:"adi"|"adx"):string{
  return `OM0RX-xLog-integrated-${new Date().toISOString().slice(0,10)}.${format}`;
}

/** Opens the destination before any record is read; null when the chooser was cancelled. */
export async function openLogbookExport(format:"adi"|"adx"):Promise<LogbookExportSink|null>{
  const filename=logbookExportFilename(format);
  if(!isTauri()){
    const parts:string[]=[];
    return {
      append:async text=>{parts.push(text);},
      finish:async(header,footer)=>saveTextFile(filename,header+parts.join("")+footer,
        format==="adx"?"application/xml;charset=utf-8":"text/plain;charset=utf-8","logbook_export_file"),
      abort:async()=>{parts.length=0;},
    };
  }
  const opened=await invoke<{cancelled:boolean;handle?:string}>("logbook_export_open",{filename});
  if(opened?.cancelled===true)return null;
  const handle=opened?.handle;
  if(typeof handle!=="string"||!handle)throw new Error("The export could not be started. Please try again.");
  let open=true;
  return {
    append:async text=>{
      if(!open)throw new Error("The export is no longer open.");
      // A rejected piece leaves the stream for abort to release at once.
      await invoke("logbook_export_append",{handle,text});
    },
    finish:async(header,footer)=>{
      if(!open)throw new Error("The export is no longer open.");
      open=false;
      const result=await invoke<{saved:boolean;cancelled:boolean}>("logbook_export_finish",{handle,header,footer});
      if(result?.saved===true&&result.cancelled===false)return "Export saved.";
      throw new Error("The export did not complete. Please try again.");
    },
    abort:async()=>{
      if(!open)return;
      open=false;
      try{await invoke("logbook_export_abort",{handle});}catch{/* the stream also expires on its own */}
    },
  };
}

export function useAnalyticsExport(){
  const active=useRef(false);
  const [pending,setPending]=useState(false),[pendingLabel,setPendingLabel]=useState(""),[message,setMessage]=useState(""),[error,setError]=useState("");
  const perform=async(label:string,waiting:string,action:()=>Promise<string>)=>{
    if(active.current)return;
    active.current=true;setPending(true);setPendingLabel(waiting);setMessage("");setError("");
    try{const outcome=await action();setMessage(outcome);return outcome;}
    catch(cause){setError(`${label} failed: ${cause instanceof Error?cause.message:String(cause)}`);}
    finally{active.current=false;setPending(false);}
  };
  const save=(filename:string,content:()=>string,type?:string)=>perform("Export","Choose where to save the export…",()=>saveAnalyticsFile(filename,content(),type));
  const print=()=>perform("Print","Opening print dialog…",async()=>{
    if(isTauri())await invoke<void>("contest_analytics_print");
    else window.print();
    return "Print dialog requested.";
  });
  return {pending,pendingLabel,message,error,save,print};
}
export type AnalyticsExport=ReturnType<typeof useAnalyticsExport>;
