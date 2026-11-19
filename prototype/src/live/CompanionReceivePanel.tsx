import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Device = { id: string; name: string; inputRate: number; inputChannels: number; dax_channel: number };
type Route = { radio_id: string; receiver_id: string; dax_channel: number; frequency_hz: number; mode: string };
type Status = { state: string; reason?: string; descriptor?: { sample_rate: number; source_channels: number; selected_channel: number } };
export default function CompanionReceivePanel() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [routes, setRoutes] = useState<Route[]>([]);
  const [deviceID, setDeviceID] = useState("");
  const [receiverID, setReceiverID] = useState("");
  const [channel, setChannel] = useState(0);
  const [confirmed, setConfirmed] = useState(false);
  const [status, setStatus] = useState<Status>({ state: "stopped" });
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const intent = useRef(0);
  const device = devices.find(d => d.id === deviceID);
  const route = routes.find(r => r.receiver_id === receiverID);
  const startHint = !route ? "Select the current FLEX slice A to start receive sharing."
    : !device ? "Select the matching DAX RX input to start receive sharing."
    : device.dax_channel !== route.dax_channel ? "Choose the DAX RX input with the same channel as the selected slice."
    : !confirmed ? "To start or restart sharing, confirm the selected DAX mapping using the checkbox above."
    : "";
  async function refresh() {
    setError("");
    try {
      const inventory = await invoke<{ devices: Device[]; routes: Route[] }>("companion_rx_request", { action: "devices" });
      setDevices(inventory.devices); setRoutes(inventory.routes); setConfirmed(false);
      setStatus(await invoke<Status>("companion_rx_request", { action: "status" }));
    } catch (e) { setError(String(e)); }
  }
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      const expected = intent.current;
      void invoke<Status>("companion_rx_request", { action: "status" }).then(value => { if (intent.current === expected) setStatus(value); }).catch(e => { if (intent.current === expected) setError(String(e)); });
    }, 2000);
    return () => window.clearInterval(timer);
  }, []);
  async function action(action: "start" | "stop") {
    const expected = ++intent.current;
    setBusy(true); setError("");
    try {
      const params = action === "start" && device && route ? {
        device_id: device.id, radio_id: route.radio_id, receiver_id: route.receiver_id,
        dax_channel: route.dax_channel, sample_rate: device.inputRate,
        source_channels: device.inputChannels, selected_channel: channel, operator_confirmed: confirmed,
      } : undefined;
      const value = await invoke<Status>("companion_rx_request", { action, params });
      if (intent.current === expected) { setStatus(value); setConfirmed(false); }
    } catch (e) { if (intent.current === expected) setError(String(e)); }
    finally { if (intent.current === expected) setBusy(false); }
  }
  return <section aria-labelledby="companion-receive-title">
    <h4 id="companion-receive-title">Share FLEX slice A receive audio</h4>
    <p>Windows DAX receive audio, owned by xLog in Normal Logging. Select the exact DAX input assigned to the observed slice. Only the selected channel is shared; there is no transmit control.</p>
    <button type="button" disabled={busy} onClick={() => void refresh()}>Refresh observed routes and inputs</button>
    <label>Observed FLEX slice A <select value={receiverID} onChange={e => { setReceiverID(e.target.value); setConfirmed(false); }}>
      <option value="">Choose current slice A</option>
      {routes.map(r => <option key={r.receiver_id} value={r.receiver_id}>{r.radio_id} · DAX {r.dax_channel} · {(r.frequency_hz / 1e6).toFixed(6)} MHz {r.mode}</option>)}
    </select></label>
    <label>Stable Windows input <select value={deviceID} onChange={e => { setDeviceID(e.target.value); setChannel(0); setConfirmed(false); }}>
      <option value="">Choose DAX RX endpoint</option>
      {devices.map(d => <option key={d.id} value={d.id}>{d.name} · {d.inputRate} Hz · {d.inputChannels} channels</option>)}
    </select></label>
    <label>Input channel <select value={channel} onChange={e => { setChannel(Number(e.target.value)); setConfirmed(false); }}>
      {Array.from({ length: device?.inputChannels ?? 1 }, (_, i) => <option key={i} value={i}>{i + 1}</option>)}
    </select></label>
    <label><input type="checkbox" checked={confirmed} onChange={e => setConfirmed(e.target.checked)} />I verified in DAX that this input is connected to this radio’s slice A and DAX channel.</label>
    <p>Capture stops after tuning, route changes or lost timing. Restart explicitly after checking the new route. Windows capture timestamps do not certify radio latency or the computer’s UTC synchronization.</p>
    <button type="button" disabled={busy || status.state === "running" || !confirmed || !device || !route || device.dax_channel !== route.dax_channel} onClick={() => void action("start")}>Start RX sharing</button>
    <button type="button" onClick={() => void action("stop")}>Stop RX sharing</button>
    {status.state !== "running" && !busy && startHint && <p>{startHint}</p>}
    <ErrorNotice><p role="status">{status.state === "running" ? `Sharing receive audio at ${status.descriptor?.sample_rate} Hz` : status.reason ?? "Receive sharing is stopped."}</p></ErrorNotice>
    {error && <ErrorNotice><p role="alert">{error}</p></ErrorNotice>}
  </section>;
}
