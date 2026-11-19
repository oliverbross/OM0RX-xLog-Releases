import { ErrorNotice } from "./live/ErrorNotice";
import { useEffect, useMemo, useRef, useState } from "react";
import geometryUrl from "../../Sources/XLogCore/Resources/dxcc-boundaries.geojson?url";
import { grayline, greatCircle, solarPosition, type Coordinate } from "./MapGeography";
import { maidenheadGrid } from "./MapGrid";
import { mapSpotDetails, type MapSpot } from "./MapSpots";

type Point = { call?: string; lon: number; lat: number };
type Status = "needed" | "worked" | "confirmed";
type Feature = { properties: { dxcc_entity_code: number; dxcc_name: string | null }; geometry: {
  type: "Polygon" | "MultiPolygon"; coordinates: number[][][] | number[][][][];
} };
type Camera = { x: number; y: number; zoom: number };
const initial: Camera = { x: 500, y: 250, zoom: 1 };
const project = ([lon, lat]: readonly number[]) => [(lon + 180) / 360 * 1000, (90 - lat) / 180 * 500];
const color = (status: Status | "unknown") => status === "needed" ? "#8fb3d9" : status === "worked" ? "#ff9d2e" : status === "confirmed" ? "#12d6a0" : "#243e54";
const clamp = (camera: Camera): Camera => {
  const zoom = Math.max(1, Math.min(16, camera.zoom));
  return { zoom, x: Math.max(500 / zoom, Math.min(1000 - 500 / zoom, camera.x)), y: Math.max(250 / zoom, Math.min(500 - 250 / zoom, camera.y)) };
};
const linePath = (lines: readonly Coordinate[][]) => lines.map(line => line.map((point, i) => `${i ? "L" : "M"}${project(point).join(",")}`).join(" ")).join(" ");

// Keep date-line polygons local, then repeat them across the world seam.
export function softwarePolygonPath(rings: number[][][]): string {
  let anchor: number | undefined;
  return rings.map(ring => {
    let previous = anchor ?? ring[0]?.[0] ?? 0;
    return ring.map(([longitude, latitude], index) => {
      let lon = longitude;
      while (lon - previous > 180) lon -= 360;
      while (lon - previous < -180) lon += 360;
      if (anchor === undefined) anchor = lon;
      previous = lon;
      return `${index ? "L" : "M"}${project([lon, latitude]).join(",")}`;
    }).join(" ") + "Z";
  }).join(" ");
}

/** Bundled geography and operating overlays when the display cannot create WebGL. */
export default function SoftwareWorldMap<T extends MapSpot>({ spots, selected, onSpot, home, target, longPath,
  overlay, statuses, statusFilter, spotColor, showGrid, showSun, showGrayline, date, actionLabel, persistenceKey,
  receivers = [], onReceiver,
}: {
  spots: T[]; selected: string | null; onSpot?: (spot: T) => void; home?: Point | null; target?: Point | null;
  longPath: boolean; overlay: "Spots" | "DXCC" | "Both"; statuses?: Record<number, Status> | null;
  statusFilter: Status[]; spotColor: (spot: T) => string; showGrid: boolean; showSun: boolean;
  showGrayline: boolean; date: Date; actionLabel: string; persistenceKey?: string;
  receivers?: { key: string; spotter: string; location: { lon: number; lat: number } }[];
  onReceiver?: (key: string) => void;
}) {
  const [features, setFeatures] = useState<Feature[]>([]), [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const key = persistenceKey ? `xlog.worldMap.${persistenceKey}.software.v1` : null;
  const [camera, setCamera] = useState<Camera>(() => {
    try {
      const saved = key ? JSON.parse(localStorage.getItem(key) || "null") as Camera | null : null;
      if (saved && [saved.x, saved.y, saved.zoom].every(Number.isFinite)) return clamp(saved);
    } catch { /* Optional view preference. */ }
    return initial;
  });
  useEffect(() => { if (key) try { localStorage.setItem(key, JSON.stringify(camera)); } catch { /* Optional view preference. */ } }, [camera, key]);
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    void fetch(geometryUrl, { signal: controller.signal }).then(response => {
      if (!response.ok) throw new Error("Bundled map geography could not be loaded.");
      return response.json();
    }).then(data => {
      if (!Array.isArray(data.features) || !data.features.length) throw new Error("Bundled map geography is empty.");
      if (!controller.signal.aborted) setFeatures(data.features);
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => controller.abort();
  }, [attempt]);
  const paths = useMemo(() => features.flatMap((feature, featureIndex) => {
    const polygons = feature.geometry.type === "Polygon" ? [feature.geometry.coordinates as number[][][]] : feature.geometry.coordinates as number[][][][];
    return polygons.map((rings, index) => ({ key: `${featureIndex}:${index}`, feature, path: softwarePolygonPath(rings) }));
  }), [features]);
  const width = 1000 / camera.zoom, height = 500 / camera.zoom;
  const left = camera.x - width / 2, top = camera.y - height / 2;
  const grid = useMemo(() => maidenheadGrid({ west: left / 1000 * 360 - 180, east: (left + width) / 1000 * 360 - 180,
    north: 90 - top / 500 * 180, south: 90 - (top + height) / 500 * 180 }, Math.log2(camera.zoom) + 1, 350), [left, top, width, height, camera.zoom]);
  const route = home && target ? greatCircle([home.lon, home.lat], [target.lon, target.lat], longPath) : [];
  const sun = solarPosition(date);
  const drag = useRef<{ x: number; y: number; camera: Camera } | null>(null);
  const selectedSpot = spots.find(spot => spot.id === selected);
  useEffect(() => {
    if (!selectedSpot) return;
    const [x, y] = project([selectedSpot.lon, selectedSpot.lat]);
    setCamera(current => clamp({ ...current, x, y }));
  }, [selectedSpot?.id, selectedSpot?.lon, selectedSpot?.lat]);
  const zoom = (factor: number) => setCamera(current => clamp({ ...current, zoom: current.zoom * factor }));
  const point = (value: Point, kind: string, label: string) => {
    const [x, y] = project([value.lon, value.lat]);
    return <g key={kind} className={`software-map-${kind}`} transform={`translate(${x} ${y})`}>
      <circle r={4 / camera.zoom} /><text x={7 / camera.zoom} y={-5 / camera.zoom} fontSize={12 / camera.zoom}>{value.call || label}</text>
    </g>;
  };
  return <div className="software-map" data-map-renderer="svg">
    <ErrorNotice><p className="software-map-notice" role="status">Compatibility map · bundled geography · satellite imagery unavailable</p></ErrorNotice>
    {error ? <ErrorNotice><p role="alert">{error} <button type="button" onClick={() => setAttempt(value => value + 1)}>Reload map</button></p></ErrorNotice> : null}
    <svg viewBox={`${left} ${top} ${width} ${height}`} role="region" tabIndex={0}
      aria-label="Interactive compatibility world map, arrow keys pan, plus and minus zoom"
      onKeyDown={event => {
        if (event.target !== event.currentTarget) return;
        if (["+", "="].includes(event.key)) zoom(1.5);
        else if (event.key === "-") zoom(1 / 1.5);
        else if (event.key.startsWith("Arrow")) setCamera(current => clamp({ ...current,
          x: current.x + (event.key === "ArrowRight" ? width / 10 : event.key === "ArrowLeft" ? -width / 10 : 0),
          y: current.y + (event.key === "ArrowDown" ? height / 10 : event.key === "ArrowUp" ? -height / 10 : 0) }));
        else return;
        event.preventDefault(); event.stopPropagation();
      }}
      onPointerDown={event => {
        if (event.button !== 0 || (event.target as Element).closest('[role="button"]')) return;
        drag.current = { x: event.clientX, y: event.clientY, camera };
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={event => {
        const start = drag.current; if (!start) return;
        const rect = event.currentTarget.getBoundingClientRect();
        const scale = Math.min(rect.width / width, rect.height / height);
        if (scale > 0) setCamera(clamp({ ...start.camera, x: start.camera.x - (event.clientX - start.x) / scale, y: start.camera.y - (event.clientY - start.y) / scale }));
      }} onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}>
      <rect x={0} y={0} width={1000} height={500} fill="#071321" />
      {[-1000, 0, 1000].map(offset => <g key={offset} transform={`translate(${offset} 0)`}>
        {paths.map(({ key: pathKey, feature, path }) => {
          const status = statuses?.[feature.properties.dxcc_entity_code] ?? "unknown";
          const displayed = overlay !== "Spots" && (status === "unknown" || statusFilter.includes(status));
          return <path key={pathKey} d={path} fill={displayed ? color(status) : "#243e54"} fillRule="evenodd"
            stroke="#50657b" strokeWidth={0.4 / camera.zoom} data-dxcc={feature.properties.dxcc_entity_code}>
            <title>{feature.properties.dxcc_name} · DXCC {feature.properties.dxcc_entity_code} · {status}</title>
          </path>;
        })}
      </g>)}
      {showGrid && <g aria-label="Maidenhead locator grid"><path d={linePath(grid.lines)} fill="none" stroke="#35c8f566" strokeWidth={0.5 / camera.zoom} />
        {grid.labels.map(label => { const [x, y] = project(label.coordinate); return <text key={label.key} x={x} y={y} textAnchor="middle" fill="#a8e8fa" fontSize={8 / camera.zoom}>{label.locator}</text>; })}</g>}
      {showGrayline && <path aria-label="Greyline" d={linePath(grayline(date))} fill="none" stroke="#a9b6c8" strokeWidth={1.5 / camera.zoom} />}
      {!!route.length && <path aria-label={longPath ? "Long great-circle path" : "Short great-circle path"} d={linePath(route)} fill="none" stroke="#f3b23c" strokeWidth={2 / camera.zoom} />}
      {showSun && point({ lon: sun[0], lat: sun[1] }, "sun", "Sun")}
      {home && point(home, "home", "Home")}{target && point(target, "target", "Target")}
      {overlay !== "DXCC" && spots.map(spot => {
        const [x, y] = project([spot.lon, spot.lat]);
        return <g key={spot.id} transform={`translate(${x} ${y})`} role="button" tabIndex={0}
          aria-label={`${spot.call} · ${mapSpotDetails(spot).join(" · ")} · ${actionLabel}`}
          className="software-map-spot" onClick={() => onSpot?.(spot)} onKeyDown={event => {
            if (event.key === "Enter" || event.key === " ") { event.preventDefault(); event.stopPropagation(); onSpot?.(spot); }
          }}>
          <title>{spot.call} · {mapSpotDetails(spot).join(" · ")}</title>
          <circle r={8 / camera.zoom} fill="transparent" />
          <circle r={(spot.id === selected ? 4 : 3) / camera.zoom} fill={spotColor(spot)} stroke={spot.id === selected ? "white" : "none"} strokeWidth={1 / camera.zoom} />
          <text x={6 / camera.zoom} y={-4 / camera.zoom} fill={spotColor(spot)} fontSize={11 / camera.zoom}>{spot.call}</text>
        </g>;
      })}
      {receivers.map(receiver => { const [x, y] = project([receiver.location.lon, receiver.location.lat]); return <g key={receiver.key} transform={`translate(${x} ${y})`}
        role="button" tabIndex={0} aria-label={`RBN receiver ${receiver.spotter}`} onClick={() => onReceiver?.(receiver.key)} onKeyDown={event => {
          if (event.key === "Enter" || event.key === " ") { event.preventDefault(); event.stopPropagation(); onReceiver?.(receiver.key); }
        }}><circle r={5 / camera.zoom} fill="#12d6a0" /><title>{receiver.spotter}</title></g>; })}
    </svg>
    <div className="software-map-navigation" aria-label="Compatibility map navigation">
      <button type="button" aria-label="Zoom in" onClick={() => zoom(1.5)}>+</button>
      <button type="button" aria-label="Zoom out" onClick={() => zoom(1 / 1.5)}>−</button>
      <button type="button" onClick={() => setCamera(initial)}>World</button>
    </div>
  </div>;
}
