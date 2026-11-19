import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import AgentLocalApi from "./AgentLocalApi";
import AgentConfiguration from "./AgentConfiguration";
import {
  ask,
  bands,
  instruments,
  type Config,
  type Instrument,
  type Settings,
  type Readings,
} from "./elecraftClient";
import ElecraftRoutes from "./ElecraftRoutes";
import ElecraftPorts from "./ElecraftPorts";
import ElecraftPowerSettings from "./ElecraftPowerSettings";
import "./ElecraftSurface.css";
import { isMacOS } from "./platform";

type Request = (method: string, params?: Record<string, any>) => Promise<Record<string, any>>;
async function agentConfiguration(request: Request | undefined, config?: Record<string, unknown>) {
  if (!isMacOS()) return invoke<Record<string, any>>("elecraft_agent", { action: config ? "save" : "load", config });
  if (!request) throw new Error("Open instrument settings from the Desk or Settings window.");
  const response = await request(config ? "station_configuration_save" : "station_configuration",
    { provider: "direct_agent", ...(config ? { configuration: config } : {}) });
  if (response.ok === false) throw new Error(response.error?.message ?? "Agent configuration is unavailable.");
  return (response.result ?? response).configuration as Record<string, any>;
}

function AgentSettings({ request }: { request?: Request }) {
  const [profile, setProfile] = useState<Record<string, unknown> | null>(null);
  const [message, setMessage] = useState("");
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    agentConfiguration(request)
      .then((p) => {
        if (active) {
          setProfile(p);
          setMessage("");
        }
      })
      .catch((e) => active && setMessage(String(e)));
    return () => {
      active = false;
    };
  }, [revision]);
  return (
    <section>
      <div className="ec-actions">
        <button type="button" onClick={() => setRevision((v) => v + 1)}>
          Reload Agent settings
        </button>
      </div>
      <ErrorNotice when={true} title="Message"><output role="status">{message}</output></ErrorNotice>
      {!profile && !isMacOS() && <AgentLocalApi revision={revision} />}
      {profile && (
        <AgentConfiguration
          grouped
          connectionTools={isMacOS() ? undefined : <AgentLocalApi revision={revision} />}
          discover={isMacOS() ? undefined : (host, port) =>
            invoke<string[]>("elecraft_agent", {
              action: "discover",
              config: { host, port },
            })
          }
          restart={isMacOS() ? undefined : () => invoke("elecraft_agent", { action: "restart" })}
          config={profile}
          onSave={async (config) => {
            const saved = await agentConfiguration(request, config);
            setProfile(saved);
          }}
        />
      )}
    </section>
  );
}

type Choice = [string, string];
function InstrumentPreferences({
  device,
  port,
  names,
}: {
  device: Instrument;
  port: string | null;
  names: string[];
}) {
  const [read, setRead] = useState<Readings>({});
  const [edits, setEdits] = useState<Readings>({});
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    setRead({});
    setEdits({});
    setMessage("");
    if (port)
      ask<Readings>("read", device)
        .then((v) => active && setRead(v))
        .catch((e) => active && setMessage(String(e)));
    return () => {
      active = false;
    };
  }, [port, device]);
  const bandOptions: Choice[] = bands.map((b, i) => [
    String(i).padStart(2, "0"),
    b,
  ]);
  const fields: [string, string, Choice[]][] =
    device === "kpa"
      ? [
          ["Band", "^BN", bandOptions],
          [
            "Minimum fan speed",
            "^FC",
            Array.from({ length: 7 }, (_, i) => [
              String(i),
              i ? String(i) : "Automatic",
            ]),
          ],
          [
            "Speaker alarm",
            "^SP",
            [
              ["0", "Off"],
              ["1", "On"],
            ],
          ],
        ]
      : [
          ["Band", "BN", bandOptions],
          [
            "Tuner mode",
            "MD",
            [
              ["A", "Automatic"],
              ["M", "Manual"],
              ["B", "Bypass"],
            ],
          ],
          [
            "Current antenna",
            "AN",
            names.map((n, i) => [String(i + 1), "ANT " + (i + 1) + " · " + n]),
          ],
          [
            "Amplifier key",
            "AMPI",
            [
              ["0", "Connected"],
              ["1", "Interrupted"],
            ],
          ],
          [
            "Attenuator",
            "ATTN",
            [
              ["0", "Off"],
              ["1", "On"],
            ],
          ],
        ];
  const current = (key: string) =>
    key === "^FC" && read[key] != null
      ? String(Number(read[key]))
      : (read[key] ?? "");
  async function save() {
    setBusy(true);
    setMessage("");
    try {
      for (const [key, value] of Object.entries(edits)) {
        await ask("control", device, { key, value });
        setRead((old) => ({ ...old, [key]: value }));
        setEdits((old) => {
          const next = { ...old };
          delete next[key];
          return next;
        });
      }
      setRead(await ask<Readings>("read", device));
      setMessage("Device settings saved and verified.");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <fieldset>
      <legend>{device === "kpa" ? "KPA500 amplifier" : "KAT500 tuner"}</legend>
      <div className="ec-hardware-options">
        {fields.map(([label, key, choices]) => (
          <label key={key}>
            {label}
            <select
              disabled={!port || busy || !read[key]}
              value={edits[key] ?? current(key)}
              onChange={(e) => {
                const v = e.target.value;
                setEdits((old) => {
                  const next = { ...old, [key]: v };
                  if (v === current(key)) delete next[key];
                  return next;
                });
              }}
            >
              <option value="" disabled>
                —
              </option>
              {choices.map(([v, t]) => (
                <option value={v} key={v}>
                  {t}
                </option>
              ))}
            </select>
          </label>
        ))}
      </div>
      <button
        type="button"
        disabled={!Object.keys(edits).length || busy}
        onClick={() => void save()}
      >
        Save {device === "kpa" ? "KPA500" : "KAT500"} settings
      </button>
      <ErrorNotice when={true} title="Message"><output role="status">{message}</output></ErrorNotice>
    </fieldset>
  );
}

export default function ElecraftSettings({ request }: { request?: Request } = {}) {
  const [agentOpen, setAgentOpen] = useState(false);
  const [status, setStatus] = useState<Settings | null>(null);
  const [cfg, setCfg] = useState<Config | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    ask<Settings>("settings")
      .then((s) => {
        if (active) {
          setStatus(s);
          setCfg(s.config);
        }
      })
      .catch((e) => active && setMessage(String(e)));
    return () => {
      active = false;
    };
  }, [revision]);
  useEffect(() => {
    let active = true;
    let pending = false;
    const refresh = async () => {
      if (pending || busy) return;
      pending = true;
      try {
        const next = await ask<Settings>("status");
        if (active)
          setStatus((old) =>
            old ? { ...old, ...next, ports: old.ports } : old,
          );
      } catch (e) {
        if (active) setMessage(String(e));
      } finally {
        pending = false;
      }
    };
    const timer = setInterval(refresh, 2500);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [busy]);
  async function operate(
    action: "save" | "connect" | "disconnect",
    device: Instrument | "",
    draft: Config,
    savedMessage = "Connection settings saved.",
  ) {
    setBusy(true);
    setMessage("");
    try {
      if (action === "save" || action === "connect")
        await ask("save", "", draft);
      if (action !== "save") await ask(action, device);
      setMessage(
        action === "save"
          ? savedMessage
          : action === "connect"
            ? "Connected."
            : "Disconnected.",
      );
    } catch (e) {
      setMessage(String(e));
    }
    try {
      const s = await ask<Settings>("settings");
      setStatus(s);
    } catch (e) {
      setMessage(String(e));
    }
    setBusy(false);
  }
  if (!cfg || !status)
    return (
      <section>
        <h2>KPA500 &amp; KAT500</h2>
        <output>{message || "Loading settings…"}</output>
        {message && (
          <button type="button" onClick={() => setRevision((v) => v + 1)}>
            Retry
          </button>
        )}
      </section>
    );
  const source = cfg.source ?? "serial",
    active = status.config.source ?? "serial";
  return (
    <div className="ec-preferences">
      <div className="ec-source">
        <h2>KPA500 &amp; KAT500</h2>
        <label>
          Instrument connection{" "}
          <select
            value={source}
            disabled={busy}
            onChange={(e) =>
              setCfg({ ...cfg, source: e.target.value as Config["source"] })
            }
          >
            <option value="serial">{isMacOS() ? "Direct serial ports" : "Direct COM ports"}</option>
            <option value="agent">{isMacOS() ? "Remote Station Agent" : "Local Station Agent"}</option>
          </select>
        </label>
      </div>
      <fieldset disabled={busy}>
        <legend>Connection settings</legend>
        {source === "serial" && (
          <ElecraftPorts cfg={cfg} setCfg={setCfg} ports={status.ports} />
        )}
        <div className="ec-actions">
          <button type="button" onClick={() => void operate("save", "", cfg)}>
            Save connection settings
          </button>
          {source === "serial" &&
            instruments.map((d) => (
              <button
                type="button"
                key={d}
                onClick={() =>
                  void operate(status[d] ? "disconnect" : "connect", d, cfg)
                }
              >
                {status[d] ? "Disconnect" : "Save & connect"}{" "}
                {d === "kpa" ? "KPA500" : "KAT500"}
                {status[d] ? " · " + status[d] : ""}
              </button>
            ))}
        </div>
        <ErrorNotice when={true} title="Message"><output role="status">{message}</output></ErrorNotice>
      </fieldset>
      {source === "serial" && active === "serial" && (
        <>
          <ElecraftPowerSettings config={cfg} onChange={setCfg} busy={busy} loadAgent={() => agentConfiguration(request)} onSave={() => void operate("save", "", cfg, "Power policy saved.")} />
          <ElecraftRoutes
            key={status.kat}
            port={status.kat}
            names={cfg.names}
          />
          {instruments.map((d) => (
            <InstrumentPreferences
              key={d + status[d]}
              device={d}
              port={status[d]}
              names={cfg.names}
            />
          ))}
        </>
      )}
      {source === "agent" ? (
        <>{isMacOS() && <p>Use the Agent connected in Radio Settings. Instrument readings follow the active radio.</p>}<AgentSettings request={request} /></>
      ) : (
        <details onToggle={(e) => setAgentOpen(e.currentTarget.open)}>
          <summary>Remote Station Agent settings</summary>
          {agentOpen && <AgentSettings request={request} />}
        </details>
      )}
    </div>
  );
}
