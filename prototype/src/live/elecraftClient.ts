import { invoke } from "@tauri-apps/api/core";
export type Instrument = "kpa" | "kat";
export type BandPower = { pa_w: number; barefoot_w: number; digital_w: number; pa_allowed: boolean; digital_pa_allowed: boolean };
export type PowerPolicy = { enabled: boolean; bands: Record<string, BandPower> };
export type Config = {
  kpa_port: string;
  kat_port: string;
  kpa_baud: number;
  kat_baud: number;
  names: string[];
  source?: "serial" | "agent";
  kpa_auto_connect?: boolean;
  kat_auto_connect?: boolean;
  follow_band?: boolean;
  power_policy?: PowerPolicy;
};
export type Settings = {
  config: Config;
  ports: string[];
  kpa: string | null;
  kat: string | null;
};
export type Route = { enabled: boolean[]; preferred: number };
export type Readings = Record<string, string>;
export const bands = [
  "160m",
  "80m",
  "60m",
  "40m",
  "30m",
  "20m",
  "17m",
  "15m",
  "12m",
  "10m",
  "6m",
];
export const instruments: Instrument[] = ["kpa", "kat"];
export const ask = <T>(action: string, device = "", params: unknown = {}) =>
  invoke<T>("elecraft_request", { action, device, params });
