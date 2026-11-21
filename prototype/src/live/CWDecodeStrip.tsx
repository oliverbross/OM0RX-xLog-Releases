import { invoke, isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import "./CWDecodeStrip.css";

export type CWDecodeSignal = { id: number; hz: number; text: string; pending: string; wpm?: number | null; snr?: number | null; call?: string | null; cq?: boolean };
export type CWDecodeView = {
  source: "agent" | "skimmer"; status: "connecting" | "connected" | "unavailable";
  message?: string | null; version?: string | null; in_coverage?: boolean | null; signal?: CWDecodeSignal | null;
};

const POLL_MS = 350;
const MAX_CHARACTERS = 160;

export const isCWMode = (mode: unknown) => typeof mode === "string" && /^CW/i.test(mode.trim());

/** Pure view model, exported for tests. */
export function cwDecodeLine(view: CWDecodeView | null): { text: string; pending: string; detail: string; idle: string | null } {
  if (!view) return { text: "", pending: "", detail: "", idle: "CW decode starting…" };
  const from = view.source === "agent" ? "Remote Station Agent" : "OM0RX-Skimmer";
  if (view.status !== "connected") return { text: "", pending: "", detail: view.message ?? "", idle: view.status === "connecting" ? "CW decode connecting…" : `CW decode unavailable (${from})` };
  if (view.in_coverage === false) return { text: "", pending: "", detail: "", idle: "Skimmer is not covering this frequency" };
  const signal = view.signal;
  if (!signal || !(signal.text || signal.pending)) return { text: "", pending: "", detail: "", idle: "No CW decoded on this frequency" };
  const text = signal.text.length > MAX_CHARACTERS ? signal.text.slice(-MAX_CHARACTERS) : signal.text;
  const detail = [signal.call, signal.wpm ? `${Math.round(signal.wpm)} WPM` : "", typeof signal.snr === "number" ? `${Math.round(signal.snr)} dB` : ""].filter(Boolean).join(" · ");
  return { text, pending: signal.pending ?? "", detail, idle: null };
}

/** Running copy of the CW signal at the radio frequency, pushed by OM0RX-Skimmer. Receive-only. */
export default function CWDecodeStrip({ radioId = "radio1", frequencyHz, mode }: { radioId?: string; frequencyHz?: number | null; mode?: string | null }) {
  const [view, setView] = useState<CWDecodeView | null>(null);
  const active = isTauri() && isCWMode(mode) && typeof frequencyHz === "number" && Number.isFinite(frequencyHz) && frequencyHz > 0;
  useEffect(() => {
    if (!active) { setView(null); return; }
    let live = true, busy = false;
    const read = async () => {
      if (busy) return;
      busy = true;
      try {
        const value = await invoke<CWDecodeView>("cw_decode_snapshot", { radioId, frequencyHz });
        if (live) setView(value);
      } catch (cause) {
        if (live) setView({ source: "skimmer", status: "unavailable", message: cause instanceof Error ? cause.message : String(cause) });
      } finally { busy = false; }
    };
    void read();
    const timer = setInterval(() => void read(), POLL_MS);
    return () => { live = false; clearInterval(timer); };
  }, [active, radioId, frequencyHz]);
  if (!active) return null;
  const line = cwDecodeLine(view);
  const title = [line.detail, view?.message, view?.source === "agent" ? "Decoded by OM0RX-Skimmer via the Remote Station Agent" : "Decoded by OM0RX-Skimmer"].filter(Boolean).join("\n");
  return <div className={`cw-decode-strip${line.idle ? " is-idle" : ""}`} role="log" aria-live="off" aria-label="Decoded CW" title={title}>
    {line.idle ? <span className="cw-decode-idle">{line.idle}</span>
      : <span className="cw-decode-text">{line.text}{line.pending && <span className="cw-decode-pending">{line.text ? " " : ""}{line.pending}</span>}</span>}
    {line.detail && <span className="cw-decode-detail">{line.detail}</span>}
  </div>;
}
