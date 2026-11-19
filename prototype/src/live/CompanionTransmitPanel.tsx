import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Status = {
  ready: boolean;
  reason: string | null;
  route: { radio_id: string; transmit_slice: string; frequency_hz: number; radio_mode: string } | null;
  identity: { mycall: string; mygrid: string } | null;
  unresolved: { operation_id: string; phase: string; outcome: string; reason: string | null }[];
};
export default function CompanionTransmitPanel() {
  const [status, setStatus] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);
  async function request(action: "status" | "disable") {
    const current = ++generation.current; setBusy(true); setError("");
    try {
      const value = await invoke<Status>("companion_tx_request", { action });
      if (current === generation.current) setStatus(value);
    } catch (e) { if (current === generation.current) setError(String(e)); }
    finally { if (current === generation.current) setBusy(false); }
  }
  useEffect(() => { void request("status"); return () => { generation.current++; }; }, []);
  return <section aria-labelledby="companion-transmit-title">
    <h4 id="companion-transmit-title">xNexus digital transmit</h4>
    <p>xNexus uses xLog’s active FLEX slice A, saved callsign and locator, and DAX TX output automatically. Use the normal transmit controls in xNexus.</p>
    <ErrorNotice><p role="status">{status ? status.ready ? "Ready for xNexus transmit." : status.reason || "Waiting for the active radio and DAX TX output." : "Reading transmit status…"}</p></ErrorNotice>
    {status?.route && <p>{status.route.radio_id} · FLEX slice {status.route.transmit_slice} · {(status.route.frequency_hz / 1e6).toFixed(6)} MHz {status.route.radio_mode} · DAX TX</p>}
    {status?.identity && <p>{status.identity.mycall} · {status.identity.mygrid}</p>}
    <button type="button" disabled={busy} onClick={() => void request("status")}>Refresh TX status</button>
    <button type="button" onClick={() => void request("disable")}>Stop TX</button>
    {status?.unresolved.map(r => <p key={r.operation_id}>{r.phase} · {r.outcome}{r.reason ? ` · ${r.reason}` : ""}</p>)}
    {error && <ErrorNotice><p role="alert">{error}</p></ErrorNotice>}
  </section>;
}
