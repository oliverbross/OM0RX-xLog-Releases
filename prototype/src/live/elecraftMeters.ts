import { bands, type Instrument, type Readings } from "./elecraftClient";
const tenths = (s?: string) =>
  !s ? "—" : s.includes(".") ? s : String(Number(s) / 10);
export function meters(d: Instrument, r: Readings): [string, string][] {
  const band = bands[Number(d === "kpa" ? r["^BN"] : r.BN)] ?? "—";
  if (d === "kpa") {
    const [watts, swr] = (r["^WS"] ?? "").split(/\s+/),
      [volts, current] = (r["^VI"] ?? "").split(/\s+/);
    return [
      ["Output", watts ? Number(watts) + " W" : "—"],
      ["SWR", watts && Number(watts) === 0 ? "No RF" : tenths(swr)],
      ["Band", band],
      [
        "Amplifier",
        r["^OS"] === "1" ? "Operate" : r["^OS"] === "0" ? "Standby" : "—",
      ],
      ["Supply", volts ? tenths(volts) + " V" : "—"],
      ["Current", current ? tenths(current) + " A" : "—"],
      ["Temperature", r["^TM"] ? Number(r["^TM"]) + " °C" : "—"],
      ["Fault", r["^FL"] ?? "—"],
      ["Minimum fan", r["^FC"] ?? "—"],
      ["Speaker", r["^SP"] === "1" ? "On" : r["^SP"] === "0" ? "Off" : "—"],
    ];
  }
  return [
    ["Antenna", r.AN ? "ANT " + r.AN : "—"],
    [
      "Mode",
      ({ A: "Automatic", M: "Manual", B: "Bypass" } as Readings)[r.MD] ?? "—",
    ],
    ["Band", band],
    ["Last SWR", r.VSWR ?? "—"],
    ["Last bypass SWR", r.VSWRB ?? "—"],
    ["Last RF", r.FC ? r.FC + " kHz" : "—"],
    ["Forward / reflected ADC", (r.VFWD ?? "—") + " / " + (r.VRFL ?? "—")],
    ["Fault", r.FLT ?? "—"],
    [
      "Amp key",
      r.AMPI === "1" ? "Interrupted" : r.AMPI === "0" ? "Connected" : "—",
    ],
    ["Tuning", r.TP === "1" ? "In progress" : r.TP === "0" ? "Idle" : "—"],
  ];
}

export type AgentReading = Record<string, string | boolean | number | null>;
export function agentMeters(
  d: Instrument,
  r: AgentReading,
): [string, string][] {
  const value = (key: string, suffix = "") =>
    r[key] == null ? "—" : String(r[key]) + suffix;
  if (d === "kpa")
    return [
      ["Output", value("output_power_w", " W")],
      ["SWR", r.swr_live === false ? "No RF" : value("swr")],
      ["Band", value("band")],
      [
        "Amplifier",
        r.operate == null ? "—" : r.operate ? "Operate" : "Standby",
      ],
      ["Temperature", value("temperature_c", " °C")],
      ["Supply", value("voltage", " V")],
      ["Current", value("current", " A")],
      ["Fault", value("fault")],
    ];
  return [
    ["Antenna", value("antenna")],
    ["Mode", value("mode")],
    ["Band", value("band")],
    ["Last SWR", value("swr_sample")],
    ["Last bypass SWR", value("bypass_swr")],
    ["Last RF", value("frequency_khz", " kHz")],
    ["Fault", value("fault")],
  ];
}
