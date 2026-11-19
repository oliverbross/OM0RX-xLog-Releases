import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ask,
  instruments,
  type Instrument,
  type Settings,
} from "./elecraftClient";
import { meters, agentMeters, type AgentReading } from "./elecraftMeters";
import "./ElecraftSidebar.css";
import { ElecraftPowerPolicyController, listenPowerPolicyOwner } from "./elecraftPowerPolicy";
import { useRef } from "react";

export type InstrumentCard = { device: Instrument; readings: [string, string][] };

export default function ElecraftSidebar({ variant = "sidebar", powerRequest, onCardsChange }: { variant?: "sidebar" | "monitor"; powerRequest?: (method: string, params?: Record<string, any>) => Promise<any>; onCardsChange?: (cards: InstrumentCard[]) => void }) {
  const power = useRef(new ElecraftPowerPolicyController()), requester = useRef(powerRequest); requester.current = powerRequest;
  const [cards, setCards] = useState<InstrumentCard[]>([]);
  const changed = useRef(onCardsChange); changed.current = onCardsChange;
  const [error, setError] = useState("");
  const lastKpaSwr = useRef<{ scope: string; value: string } | null>(null);
  useEffect(() => powerRequest ? listenPowerPolicyOwner() : undefined, [!!powerRequest]);
  useEffect(() => {
    let active = true, failures = 0;
    let timer: ReturnType<typeof setTimeout>;
    const kpaReadings = (readings: [string, string][], scope: string, idle: boolean, sample: number): [string, string][] => {
      if (lastKpaSwr.current?.scope !== scope) lastKpaSwr.current = null;
      if (Number.isFinite(sample) && sample >= 1) lastKpaSwr.current = { scope, value: String(sample) };
      return readings.map(([label, value]) => label === "SWR" && idle ? ["Last SWR", lastKpaSwr.current?.value ?? "—"] : [label, value]);
    };
    async function refresh() {
      try {
        const status = await ask<Settings>("status");
        const next: InstrumentCard[] = [];
        let amplifier: Record<string, string> | null = null;
        if (status.config.source === "agent") {
          const state = await invoke<Record<string, AgentReading>>(
            "elecraft_agent",
            { action: "meters" },
          );
          for (const device of instruments) {
            const r = state[device === "kpa" ? "kpa500" : "kat500"];
            if (
              r?.connected === true &&
              r.responsive !== false &&
              r.quality !== "stale"
            ) {
              const readings = agentMeters(device, r);
              next.push({ device, readings: device === "kpa" ? kpaReadings(readings, "agent", r.swr_live === false, Number(r.swr)) : readings });
            }
          }
        } else {
          for (const device of instruments) {
            if (!status[device]) continue;
            try {
              const data = await ask<Record<string, string>>("read", device);
              if (device === "kpa") amplifier = data;
              let readings = meters(device, data);
              if (device === "kpa") {
                const [watts, swr = ""] = (data["^WS"] ?? "").split(/\s+/);
                readings = kpaReadings(readings, `serial:${status.kpa}:${status.config.kpa_baud}`, !!watts && Number(watts) === 0, Number(swr) / (swr.includes(".") ? 1 : 10));
              }
              if (device === "kat" && data.AN) {
                const name = status.config.names[Number(data.AN) - 1];
                if (name)
                  readings[0] = ["Antenna", "ANT " + data.AN + " · " + name];
              }
              next.push({ device, readings });
            } catch {
              /* The other connected instrument remains visible. */
            }
          }
        }
        if (active && requester.current && status.config.power_policy?.enabled) {
          const selectedRequest = requester.current;
          try { await power.current.update(status, amplifier, selectedRequest, ask, () => active && requester.current === selectedRequest); }
          catch { /* Retain telemetry; the context is not replayed after an uncertain write. */ }
        }
        if (!next.some(card => card.device === "kpa")) lastKpaSwr.current = null;
        failures = 0;
        if (active) { setCards(next); changed.current?.(next); }
      } catch {
        // Keep the cards through one missed read; clear them once the
        // instruments have really stopped answering.
        failures += 1;
        if (active && failures >= 3) { lastKpaSwr.current = null; setCards([]); changed.current?.([]); }
      } finally {
        if (active) timer = setTimeout(refresh, 2500);
      }
    }
    void refresh();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, []);
  if (!cards.length) return null;
  return (
    <div
      className={"sidebar-instruments " + (variant === "monitor" ? "monitor-instruments" : "")}
      aria-label="Connected Elecraft instruments"
    >
      {cards.map(card => <ElecraftInstrumentCard key={card.device} card={card} variant={variant} onError={setError} />)}
      {error && <ErrorNotice when={Boolean(error) || undefined}><p role="status">{error}</p></ErrorNotice>}
    </div>
  );
}

/** Canvas cards reuse the sidebar owner's observation; mounting one never polls. */
export function ElecraftInstrumentCard({ card: { device, readings }, variant = "sidebar", onError }: {
  card: InstrumentCard; variant?: "sidebar" | "monitor"; onError?: (error: string) => void;
}) {
  const [error, setError] = useState("");
  const values = new Map(readings);
  const swrField = values.has("Last SWR") ? "Last SWR" : "SWR";
  const fields =
    variant === "monitor"
      ? device === "kpa"
        ? ["Output", swrField, "Amplifier", "Temperature", "Supply", "Fault"]
        : ["Antenna", "Mode", "Band", "Last SWR", "Last bypass SWR", "Fault"]
      : device === "kpa"
        ? ["Output", "Amplifier", "Temperature", swrField]
        : ["Antenna", "Last SWR", "Mode", "Band"];
  return (
    <section key={device} className={"sidebar-instrument " + device}>
      <button
        type="button"
        onClick={() =>
          void invoke("open_detached_map", { kind: "elecraft" }).catch(
            (e) => { setError(String(e)); onError?.(String(e)); },
          )
        }
        title="Open KPA500 & KAT500"
      >
        <span>{device === "kpa" ? "KPA500" : "KAT500"}</span>
        <span className="instrument-connected">Connected</span>
      </button>
      <dl>
        {fields.map((field) => (
          <div key={field}>
            <dt>{field}</dt>
            <dd>{values.get(field) ?? "—"}</dd>
          </div>
        ))}
      </dl>
      {error && !onError && <ErrorNotice when={Boolean(error) || undefined}><p role="status">{error}</p></ErrorNotice>}
    </section>
  );
}
