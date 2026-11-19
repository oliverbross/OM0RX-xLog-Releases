import { bandForRadio } from "./band";
import type { PowerPolicy, Readings, Settings } from "./elecraftClient";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
type State = Record<string, any>;
type Request = (method: string, params?: State) => Promise<State>;
type InstrumentRequest = <T>(action: string, device?: string, params?: unknown) => Promise<T>;
let policyRevision = 0, policyStatus = "Waiting for a connected radio and amplifier in receive.";
const statusListeners = new Set<() => void>();
const receiveStatus = (message: string) => { policyStatus = message; statusListeners.forEach(listener => listener()); };
const publish = (message: string) => { receiveStatus(message); void emit("xlog:power-policy-status", message).catch(() => {}); };
export const powerPolicyStatus = () => policyStatus;
export const subscribePowerPolicy = (listener: () => void) => {
  statusListeners.add(listener); let active = true, stop = () => {};
  void listen<string>("xlog:power-policy-status", event => receiveStatus(event.payload)).then(dispose => {
    if (!active) { dispose(); return; } stop = dispose;
    void emitTo("main", "xlog:power-policy-query").catch(() => {});
  }).catch(() => {});
  return () => { active = false; stop(); statusListeners.delete(listener); };
};
export const retryPowerPolicy = () => { void emitTo("main", "xlog:power-policy-retry").catch(() => {}); };
export function listenPowerPolicyOwner() {
  let active = true; const stops: (() => void)[] = [];
  for (const [event, action] of [["xlog:power-policy-query", () => publish(policyStatus)], ["xlog:power-policy-retry", () => { policyRevision++; publish("Checking the current radio and amplifier before applying the saved policy…"); }]] as const) {
    void listen(event, action).then(stop => active ? stops.push(stop) : stop()).catch(() => {});
  }
  return () => { active = false; stops.forEach(stop => stop()); };
}

export function powerPolicyTarget(policy: PowerPolicy, radio: State, amplifier: Readings | null) {
  const tx = radio.vfos?.find((v: State) => v.transmit === true);
  if (!policy.enabled || radio.observed !== true || radio.connected !== true || radio.transmitting !== false ||
      !tx || !Number.isFinite(tx.frequency_hz) || typeof tx.mode !== "string") return null;
  const band = bandForRadio(tx.frequency_hz), row = band && policy.bands[band];
  if (!row || [row.pa_w, row.barefoot_w, row.digital_w].some(w => !Number.isInteger(w) || w < 1 || w > 100)) return null;
  const mode = tx.mode.toUpperCase(), digital = !["CW", "CWR", "USB", "LSB", "SSB", "AM", "SAM", "FM", "NFM"].includes(mode);
  if (amplifier && (!["0", "1"].includes(amplifier["^OS"]) || amplifier["^FL"] !== "00")) return null;
  const operate = amplifier?.["^OS"] === "1", allowed = row.pa_allowed && (!digital || row.digital_pa_allowed);
  return { band, mode, frequency: tx.frequency_hz, vfo: tx.letter, standby: operate && !allowed,
    watts: digital ? row.digital_w : operate && allowed ? row.pa_w : row.barefoot_w };
}

/** One native Desk owner. Readbacks are checked before each step; no failed or
 * uncertain write is automatically repeated until the operating context changes. */
export class ElecraftPowerPolicyController {
  private attempted = "";
  async update(status: Settings, amplifier: Readings | null, request: Request, instrument: InstrumentRequest, active: () => boolean) {
    try { await this.apply(status, amplifier, request, instrument, active); }
    catch (cause) {
      if (active()) publish("Power policy paused. Check the radio and amplifier, then use Retry power policy.");
      throw cause;
    }
  }
  private async apply(status: Settings, amplifier: Readings | null, request: Request, instrument: InstrumentRequest, active: () => boolean) {
    const policy = status.config.power_policy;
    if (status.config.source === "agent" || !policy?.enabled || status.config.kpa_port && (!status.kpa || !amplifier)) { this.attempted = ""; return; }
    const snapshot = await request("station_snapshot", { provider: "direct_flex_lan" });
    if (!active() || snapshot.source !== "direct_flex_lan") return;
    const target = powerPolicyTarget(policy, snapshot.radio ?? {}, amplifier);
    if (!target) return;
    const key = JSON.stringify([policyRevision, status.config, snapshot.radio_id, snapshot.generation, target.band, target.mode, target.vfo, amplifier?.["^OS"]]);
    if (key === this.attempted) return;
    this.attempted = key;
    const current = await instrument<Settings>("status");
    if (!active() || JSON.stringify(current.config) !== JSON.stringify(status.config) || current.kpa !== status.kpa) return;
    if (target.standby) {
      await instrument("policy_standby", "kpa", { config: status.config });
      if (!active()) return;
    }
    // Always ask the worker to fence the exact TX slice/mode/frequency observed
    // above. The worker checks again after its receive-state await.
    const result = await request("station_radio_control", { radio_id: snapshot.radio_id, provider: "direct_flex_lan", control: "set_rf_power", watts: target.watts,
      expected_generation: snapshot.generation, expected_tx_frequency_hz: target.frequency,
      expected_tx_mode: target.mode, expected_tx_vfo: target.vfo });
    if (result.confirmed !== true || result.state?.radio?.rf_power_level !== target.watts) throw new Error("Radio drive readback was not confirmed.");
    if (active()) publish(`${target.band} ${target.mode} · Radio drive confirmed at ${target.watts} W${target.standby ? " · PA in standby" : ""}.`);
  }
}
