import { useEffect, useRef, useState } from "react";
import { ErrorNotice } from "./ErrorNotice";
import { invoke } from "@tauri-apps/api/core";

type Config = { radio_id: string; slice: string; rx_device_id: string; rx_channel: number; tx_device_id: string };
type Status = {
  config: Config | null;
  slices: { radio_id: string; slice: string; label: string }[];
  rx_devices: { id: string; name: string; channels: number }[];
  tx_devices: { id: string; name: string }[];
  state: "ready" | "waiting" | "unconfigured";
  message: string | null;
};
const empty: Config = { radio_id: "", slice: "", rx_device_id: "", rx_channel: 0, tx_device_id: "" };

export default function CompanionAudioPanel() {
  const [status, setStatus] = useState<Status | null>(null);
  const [draft, setDraft] = useState<Config>(empty);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const active = useRef(false);
  const savingNow = useRef(false);
  const generation = useRef(0);

  useEffect(() => {
    active.current = true;
    generation.current++;
    let reading = false;
    let hydrated = false;
    async function refresh(action: "devices" | "status") {
      if (reading || savingNow.current) return;
      reading = true;
      const expected = generation.current;
      try {
        const value = await invoke<Status>("companion_audio_request", { action, params: null });
        if (!active.current || expected !== generation.current) return;
        setStatus(value);
        if (!hydrated) { hydrated = true; setDraft(value.config ?? empty); }
      } catch (failure) {
        if (active.current && expected === generation.current) setError(String(failure));
      } finally { reading = false; }
    }
    void refresh("devices");
    const timer = window.setInterval(() => { void refresh("status"); }, 2000);
    return () => { active.current = false; generation.current++; window.clearInterval(timer); };
  }, []);

  async function change(next: Config) {
    if (savingNow.current || !status) return;
    setDraft(next);
    const rx = status.rx_devices.find(device => device.id === next.rx_device_id);
    if (!status.slices.some(slice => slice.radio_id === next.radio_id && slice.slice === next.slice)
      || !rx || !Number.isInteger(next.rx_channel) || next.rx_channel < 0 || next.rx_channel >= rx.channels
      || !status.tx_devices.some(device => device.id === next.tx_device_id)) return;
    savingNow.current = true; setSaving(true); setError("");
    const expected = ++generation.current;
    try {
      const value = await invoke<Status>("companion_audio_request", { action: "save", params: next });
      if (active.current && expected === generation.current) setStatus(value);
    } catch (failure) {
      if (active.current && expected === generation.current) setError(String(failure));
    } finally {
      savingNow.current = false;
      if (active.current && expected === generation.current) setSaving(false);
    }
  }

  const rx = status?.rx_devices.find(device => device.id === draft.rx_device_id);
  const sliceValue = draft.radio_id && draft.slice ? `${draft.radio_id}/${draft.slice}` : "";
  const slicePresent = status?.slices.some(slice => slice.radio_id === draft.radio_id && slice.slice === draft.slice);
  const txPresent = status?.tx_devices.some(device => device.id === draft.tx_device_id);
  return <div>
    <label>Slice <select disabled={!status || saving} value={sliceValue} onChange={event => {
      const slice = status?.slices.find(item => `${item.radio_id}/${item.slice}` === event.target.value);
      void change({ ...draft, radio_id: slice?.radio_id ?? "", slice: slice?.slice ?? "" });
    }}>
      <option value="">Choose slice</option>
      {sliceValue && !slicePresent && <option value={sliceValue}>{draft.radio_id} · Slice {draft.slice} (unavailable)</option>}
      {status?.slices.map(slice => <option key={`${slice.radio_id}/${slice.slice}`} value={`${slice.radio_id}/${slice.slice}`}>{slice.label}</option>)}
    </select></label>
    <label>RX DAX <select disabled={!status || saving} value={draft.rx_device_id} onChange={event => void change({ ...draft, rx_device_id: event.target.value, rx_channel: 0 })}>
      <option value="">Choose RX DAX</option>
      {draft.rx_device_id && !rx && <option value={draft.rx_device_id}>Saved RX DAX (unavailable)</option>}
      {status?.rx_devices.map(device => <option key={device.id} value={device.id}>{device.name}</option>)}
    </select></label>
    <label>Input channel <select disabled={!rx || saving} value={draft.rx_channel} onChange={event => void change({ ...draft, rx_channel: Number(event.target.value) })}>
      {(!rx || draft.rx_channel >= rx.channels) && <option value={draft.rx_channel}>{draft.rx_channel + 1} (unavailable)</option>}
      {Array.from({ length: rx?.channels ?? 0 }, (_, channel) => <option key={channel} value={channel}>{channel + 1}</option>)}
    </select></label>
    <label>TX DAX <select disabled={!status || saving} value={draft.tx_device_id} onChange={event => void change({ ...draft, tx_device_id: event.target.value })}>
      <option value="">Choose TX DAX</option>
      {draft.tx_device_id && !txPresent && <option value={draft.tx_device_id}>Saved TX DAX (unavailable)</option>}
      {status?.tx_devices.map(device => <option key={device.id} value={device.id}>{device.name}</option>)}
    </select></label>
    <ErrorNotice><p role="status">{saving ? "Saving audio connections…" : status?.message || (status?.state === "ready" ? "Connected and ready for xNexus." : status?.state === "unconfigured" ? "Choose your slice and DAX audio connections." : "Waiting for the radio and audio connections…")}</p></ErrorNotice>
    {error && <ErrorNotice><p role="alert">{error}</p></ErrorNotice>}
  </div>;
}
