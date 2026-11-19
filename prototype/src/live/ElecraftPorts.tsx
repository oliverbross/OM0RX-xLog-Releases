import type { Config } from "./elecraftClient";
import { instruments } from "./elecraftClient";
import { isMacOS } from "./platform";

export default function ElecraftPorts({
  cfg,
  setCfg,
  ports,
}: {
  cfg: Config;
  setCfg: (cfg: Config) => void;
  ports: string[];
}) {
  return (
    <>
      {" "}
      <div className="ec-settings">
        {instruments.map((d) => (
          <div key={d}>
          <label>
            {d === "kpa" ? "Amplifier" : "Tuner"} serial port
            <span>
              <input
                list="ec-ports"
                placeholder={isMacOS() ? "/dev/cu.usbserial…" : "COM port"}
                value={cfg[(d + "_port") as "kpa_port" | "kat_port"]}
                onChange={(e) =>
                  setCfg({
                    ...cfg,
                    [d + "_port"]: isMacOS() ? e.target.value : e.target.value.toUpperCase(),
                  })
                }
              />
              <select
                aria-label={d + " baud rate"}
                value={cfg[(d + "_baud") as "kpa_baud" | "kat_baud"]}
                onChange={(e) =>
                  setCfg({
                    ...cfg,
                    [d + "_baud"]: Number(e.target.value),
                  })
                }
              >
                {[4800, 9600, 19200, 38400, 57600, 115200].map((n) => (
                  <option key={n}>{n}</option>
                ))}
              </select>
            </span>
          </label>
            <label>
              <input type="checkbox" checked={cfg[d === "kpa" ? "kpa_auto_connect" : "kat_auto_connect"] !== false}
                onChange={e => setCfg({...cfg, [d === "kpa" ? "kpa_auto_connect" : "kat_auto_connect"]: e.target.checked})} />
              Connect {d === "kpa" ? "KPA500" : "KAT500"} at startup
            </label>
          </div>
        ))}
        <label className="ec-follow-band">
          <input type="checkbox" checked={cfg.follow_band !== false}
            onChange={e => setCfg({ ...cfg, follow_band: e.target.checked })} />
          Set the KPA500 and KAT500 band when the radio changes band (never while transmitting)
        </label>
        <datalist id="ec-ports">
          {ports.map((p) => (
            <option key={p}>{p}</option>
          ))}
        </datalist>
      </div>
      <div className="ec-settings">
        {cfg.names.map((n, i) => (
          <label key={i}>
            ANT {i + 1} name
            <input
              value={n}
              maxLength={48}
              onChange={(e) =>
                setCfg({
                  ...cfg,
                  names: cfg.names.map((x, j) =>
                    j === i ? e.target.value : x,
                  ),
                })
              }
            />
          </label>
        ))}
      </div>
    </>
  );
}
