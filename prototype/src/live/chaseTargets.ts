import type { DxAlertsSnapshot, DxAlertRule } from "./dxAlerts";
import { saveDxAlerts } from "./dxAlerts";

export const chaseBands = ["160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m", "2m", "70cm"];
export const chaseModes = ["CW", "SSB", "AM", "FM", "RTTY", "FT8", "FT4", "PSK", "DATA"];
export const targetCells = (rule: DxAlertRule) => rule.bands.flatMap(band => (rule.chaseModes ?? []).map(mode => ({ band, mode, key: `${rule.id}|${band}|${mode}` })));
export function chaseMode(mode: string): string {
  const value = mode.toUpperCase();
  return ["USB", "LSB", "PHONE"].includes(value) ? "SSB" : value === "CWR" ? "CW" : value;
}
export function chaseRule(call: string, bands: string[], modes: string[], id: string = crypto.randomUUID()): DxAlertRule {
  call = call.trim().toUpperCase();
  if (!call) throw Error("Type a callsign before saving, for example VK8RX.");
  if (!/^(?=.*[A-Z])(?=.*\d)[A-Z0-9/]{2,24}$/.test(call)) throw Error("Enter an exact callsign, for example VK8RX.");
  if (!bands.length || !modes.length || bands.some(b => !chaseBands.includes(b)) || modes.some(m => !chaseModes.includes(m))) throw Error("Choose at least one band and one mode.");
  return { id, name: call, enabled: true, sources: ["cluster"], callsignGlob: call, bands: [...new Set(bands)], chaseModes: [...new Set(modes)], modes: [], commentContains: "", callStatuses: [], dxccStatuses: [], specials: [] };
}
/** Preserve unrelated rules and use the native generation/revision checked owner. */
export function saveChaseTarget(snapshot: DxAlertsSnapshot, rule: DxAlertRule) {
  const { generation: _, revision: __, ...configuration } = snapshot.configuration;
  const same = (left: string[], right: string[]) => left.length === right.length && left.every(value => right.includes(value));
  const prior = configuration.rules.find(item => item.id === rule.id || item.callsignGlob === rule.callsignGlob && item.chaseModes?.length && same(item.bands, rule.bands) && same(item.chaseModes, rule.chaseModes ?? []));
  let rules = prior ? configuration.rules.map(item => item.id === prior.id ? { ...rule, id: prior.id } : item) : [...configuration.rules, rule];
  if (rules.length > snapshot.limits.maximumRules) throw Error("The alert list is full. Remove an unused target first.");
  if (!configuration.enabled) rules = rules.map(item => item.chaseModes?.length ? item : { ...item, enabled: false });
  return saveDxAlerts(snapshot, { ...configuration, rules, enabled: true, nativePopup: true });
}
