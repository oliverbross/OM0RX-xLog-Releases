import { ErrorNotice } from "./ErrorNotice";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import {
  ask,
  instruments,
  type Settings,
  type Instrument,
  type Readings,
} from "./elecraftClient";
import { meters, agentMeters, type AgentReading } from "./elecraftMeters";
import { InstrumentGauges } from "./InstrumentGauges";
import "./ElecraftSurface.css";

export function ElecraftSurface() {
  const [status, setStatus] = useState<Settings | null>(null);
  const [data, setData] = useState<Record<Instrument, Readings>>({
    kpa: {},
    kat: {},
  });
  const [agent, setAgent] = useState<Record<string, AgentReading>>({});
  const [messages, setMessages] = useState({ kpa: "", kat: "", config: "" });
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const linked = { kpa: status?.kpa, kat: status?.kat };
  const remote = status?.config.source === "agent";
  const msg = (d: keyof typeof messages, text: string) =>
    setMessages((old) => ({ ...old, [d]: text }));
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        if (lock.current) return;
        const next = await ask<Settings>("status");
        if (!active) return;
        setStatus(next);
        if (next.config.source === "agent") {
          const result = await invoke<Record<string, AgentReading>>(
            "elecraft_agent",
            { action: "meters" },
          );
          if (active) setAgent(result);
        } else {
          for (const d of instruments) {
            try {
              const r = next[d] ? await ask<Readings>("read", d) : {};
              if (active) setData((old) => ({ ...old, [d]: r }));
            } catch (e) {
              if (active) {
                setData((old) => ({ ...old, [d]: {} }));
                msg(d, String(e));
              }
            }
          }
        }
        if (active) msg("config", "");
      } catch (e) {
        if (active) {
          setStatus(null);
          setAgent({});
          setData({ kpa: {}, kat: {} });
          msg("config", String(e));
        }
      } finally {
        if (active) timer = setTimeout(poll, 2500);
      }
    };
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, []);
  async function act(d: Instrument, fn: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true;
    setBusy(true);
    msg(d, "");
    try {
      await fn();
    } catch (e) {
      msg(d, String(e));
    } finally {
      lock.current = false;
      setBusy(false);
    }
  }
  const read = async (d: Instrument) => {
    const r = await ask<Readings>("read", d);
    setData((old) => ({ ...old, [d]: r }));
  };
  const connect = (d: Instrument) =>
    act(d, async () => {
      await ask(linked[d] ? "disconnect" : "connect", d);
      const next = await ask<Settings>("status");
      setStatus(next);
      if (next[d]) await read(d);
      else setData((old) => ({ ...old, [d]: {} }));
    });
  const control = (d: Instrument, key: string, value = "") =>
    act(d, async () => {
      msg(d, await ask<string>("control", d, { key, value }));
      await read(d);
    });
  const buttons = (d: Instrument, items: [string, string, string][]) =>
    items.map(([label, key, val]) => (
      <button
        key={label}
        disabled={!linked[d] || busy}
        onClick={() => void control(d, key, val)}
      >
        {label}
      </button>
    ));
  return (
    <main className="elecraft">
      <div className="ec-pair">
        {instruments.map((d) => {
          const r = agent[d === "kpa" ? "kpa500" : "kat500"] ?? {};
          const connected = remote
            ? r.connected === true &&
              r.responsive !== false &&
              r.quality !== "stale"
            : !!linked[d] && Object.keys(data[d]).length > 0;
          const items = remote
            ? agentMeters(d, connected ? r : {})
            : meters(d, linked[d] ? data[d] : {});
          return (
            <section key={d} className={"ec-device ec-" + d}>
              <div className="ec-heading">
                <div>
                  <small>
                    {d === "kpa" ? "Power amplifier" : "Antenna tuner"}
                  </small>
                  <h2>{d === "kpa" ? "KPA500" : "KAT500"}</h2>
                </div>
                <span className={"ec-state " + (connected ? "ec-online" : "")}>
                  {connected
                    ? "Connected"
                    : remote || linked[d]
                      ? "No telemetry"
                      : "Disconnected"}
                </span>
              </div>
              <InstrumentGauges device={d} items={items} />
              <dl>
                {items.map(([label, value]) => (
                  <div key={label}>
                    <dt>{label}</dt>
                    <dd>{value}</dd>
                  </div>
                ))}
              </dl>
              {!remote && (
                <div className="ec-actions">
                  <button
                    disabled={busy || !status}
                    onClick={() => void connect(d)}
                  >
                    {linked[d] ? "Disconnect" : "Connect"}
                  </button>
                  {buttons(
                    d,
                    d === "kpa"
                      ? [
                          ["Power on", "^ON", "1"],
                          ["Power off", "^ON", "0"],
                          ["Standby", "^OS", "0"],
                          ["Operate", "^OS", "1"],
                          ["Clear fault", "clear", ""],
                        ]
                      : [
                          ["Power on", "PS", "1"],
                          ["Power off", "PS", "0"],
                          ["Tune", "tune", ""],
                          ["Cancel tune", "cancel", ""],
                          ["Clear fault", "clear", ""],
                        ],
                  )}
                </div>
              )}
              <ErrorNotice><output role="status">{messages[d]}</output></ErrorNotice>
            </section>
          );
        })}
      </div>
      <ErrorNotice><output role="status">{messages.config}</output></ErrorNotice>
    </main>
  );
}
