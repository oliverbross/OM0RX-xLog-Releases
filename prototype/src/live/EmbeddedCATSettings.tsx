import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useState } from "react";
import type { RadioID } from "./stationSession";

type Data = Record<string, any>;
type Props = { radioId: RadioID; snapshot?: Data | null; request: (method: string, params: Data) => Promise<Data> };

export function EmbeddedCATSettings({ radioId, snapshot, request }: Props) {
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState(snapshot?.cat_servers);
  const connected = snapshot?.source === "direct_flex_lan" && snapshot?.radio?.connected === true;
  useEffect(() => { setStatus(snapshot?.cat_servers); }, [snapshot?.cat_servers?.cat_port, snapshot?.cat_servers?.rigctl_port, connected]);
  async function apply(fields: Data) {
    setBusy(true);
    try {
      const result = await request("station_cat_servers", { provider: "direct_flex_lan", ...fields });
      setStatus(result.cat_servers);
      setNotice(fields.cat_port || fields.rigctl_port ? "Listeners started." : "Listeners stopped.");
    } catch (cause) {
      setNotice(cause instanceof Error ? cause.message : "Could not change CAT listeners.");
      try { setStatus((await request("station_snapshot", { provider: "direct_flex_lan" })).cat_servers); } catch { setStatus(undefined); }
    } finally { setBusy(false); }
  }
  const catKey = `xlog.flex.cat.${radioId}`;
  const [catPort, setCatPort] = useState(() => localStorage.getItem(catKey + ".ts480") ?? "5002");
  const [rigPort, setRigPort] = useState(() => localStorage.getItem(catKey + ".rigctl") ?? "4532");
  const [catTransmit, setCatTransmit] = useState(false);
  const validPort = (port: string) => !port || /^\d{1,5}$/.test(port) && Number(port) > 0 && Number(port) <= 65535;
  const validListeners = validPort(catPort) && validPort(rigPort) && Boolean(catPort || rigPort) && Number(catPort) !== Number(rigPort);
  const savePorts = () => {
    localStorage.setItem(catKey + ".ts480", catPort);
    localStorage.setItem(catKey + ".rigctl", rigPort);
    setNotice("CAT listener ports saved.");
  };
  return (
      <details className="radio-settings-disclosure live-station-card"><summary>Embedded CAT and RigCtl</summary>
        <p>Connect your logging or digital software to 127.0.0.1. Leave a port blank to turn that listener off.</p>
        <div className="flex-connect-fields">
          <label>TS-480 CAT port<input inputMode="numeric" value={catPort} onChange={e => setCatPort(e.target.value)} /></label>
          <label>RigCtl port<input inputMode="numeric" value={rigPort} onChange={e => setRigPort(e.target.value)} /></label>
        </div>
        <label><input type="checkbox" checked={catTransmit && snapshot?.allow_transmit === true} disabled={!snapshot?.allow_transmit} onChange={e => setCatTransmit(e.target.checked)} />Allow CAT transmit on this connection</label>
        <p>TS-480 CAT · {status?.cat_port || "Off"} · RigCtl · {status?.rigctl_port || "Off"}</p>
        <div className="live-actions">
          <button disabled={busy || !validListeners} onClick={savePorts}>Save ports</button>
          <button disabled={busy || !connected || !validListeners} onClick={() => { savePorts(); void apply({ cat_port: catPort ? Number(catPort) : null, rigctl_port: rigPort ? Number(rigPort) : null, allow_transmit: catTransmit && snapshot?.allow_transmit === true }); }}>Save and start listeners</button>
          <button disabled={busy || !(status?.cat_port || status?.rigctl_port)} onClick={() => void apply({ cat_port: null, rigctl_port: null, allow_transmit: false })}>Stop listeners</button>
        </div>
        {notice && <ErrorNotice when={true} title="Message"><p role="status">{notice}</p></ErrorNotice>}
      </details>
  );
}
