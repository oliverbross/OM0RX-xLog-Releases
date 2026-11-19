import { ErrorNotice } from "./ErrorNotice";
import { useState } from "react";
import BandMaps, { BAND_MAP_MODES, BAND_ORDER, type BandMapMode, type BandMapViewPreferences } from "./BandMaps";
import { mapRadioFrequency } from "./mapContext";
import { useMapContext } from "./useMapContext";
import { SpotActivationStatus } from "./SpotActivationStatus";
import { StableFeedbackLayer } from "./StableFeedbackLayer";

const roamingBandKey = "xlog.detached.band-map.choose-band.band";
export function detachedBand(search: string): string {
  const band = new URLSearchParams(search).get("band");
  if (band && BAND_ORDER.includes(band)) return band;
  try { const saved = localStorage.getItem(roamingBandKey); if (saved && BAND_ORDER.includes(saved)) return saved; } catch { /* Default below. */ }
  return "20m";
}
/** Window ▸ Band Maps ▸ "20m Band Map" opens with ?band= and stays on that
 *  band (its title and menu item name it). "Band Map (choose band)" has no
 *  band in its address and may switch freely. */
export function bandIsFixed(search: string): boolean {
  const band = new URLSearchParams(search).get("band");
  return !!band && BAND_ORDER.includes(band);
}
function readModes(band: string): BandMapMode[] {
  try {
    const modes: unknown = JSON.parse(localStorage.getItem(`xlog.detached.band-map.${band}.modes`) || "null");
    if (Array.isArray(modes) && modes.every(mode => BAND_MAP_MODES.includes(mode))) return [...new Set(modes)] as BandMapMode[];
  } catch { /* Unavailable storage must not interrupt the map. */ }
  return [...BAND_MAP_MODES];
}
export function DetachedBandMapsSurface() {
  const view = useMapContext();
  const [band, setBand] = useState(() => detachedBand(window.location.search));
  const fixed = bandIsFixed(window.location.search);
  const [modes, setModes] = useState(() => readModes(band));
  const preferences: BandMapViewPreferences = { bands: [band], followRadio: false, layout: "row", fontScale: 1, modes };
  const change = (next: BandMapViewPreferences) => {
    const modes = next.modes ?? [...BAND_MAP_MODES];
    setModes(modes);
    try { localStorage.setItem(`xlog.detached.band-map.${band}.modes`, JSON.stringify(modes)); } catch { /* Keep session settings if persistence is unavailable. */ }
  };
  const changeBand = (next: string) => {
    if (fixed || !BAND_ORDER.includes(next)) return;
    setBand(next);
    setModes(readModes(next));
    try { localStorage.setItem(roamingBandKey, next); } catch { /* Keep the session band. */ }
  };
  return <div className="app live monitor-app detached-map-app detached-band-map-app"><main className="live-feedback-host">
    <StableFeedbackLayer>{view.error && <ErrorNotice><p className="live-error" role="alert">{view.error}</p></ErrorNotice>}
      <SpotActivationStatus status={view.activation.status} error={view.activation.error} checking={view.activation.requesting} reconcile={() => void view.activation.reconcile()} /></StableFeedbackLayer>
    <BandMaps focusedBand={band} onFocusedBandChange={changeBand} spots={view.spots} onSelect={view.activate} radioFrequencyHz={view.context ? mapRadioFrequency(view.context) : undefined}
      preferences={preferences} onPreferencesChange={change} />
  </main></div>;
}
