import { panelMinimum, type LayoutPreset, type PanelLayout, type LayoutRect } from "./workspaceLayoutSession";

export const resizeEdges = ["n", "ne", "e", "se", "s", "sw", "w", "nw"] as const;
export type ResizeEdge = typeof resizeEdges[number];
export type CanvasBounds = { width: number; height: number; snap: boolean };

const snapped = (value: number, snap: boolean) => Math.round(value / (snap ? 8 : 1)) * (snap ? 8 : 1);
const clamped = (value: number, minimum: number, maximum: number) => Math.max(minimum, Math.min(maximum, value));

/** Shared bounded-canvas movement used by every arrangeable workspace. */
export function moveCanvasRect(rect: LayoutRect, canvas: CanvasBounds, dx: number, dy: number): LayoutRect {
  return { ...rect,
    x: clamped(snapped(rect.x + dx, canvas.snap), 0, canvas.width - rect.width),
    y: clamped(snapped(rect.y + dy, canvas.snap), 0, canvas.height - rect.height) };
}

/** Shared edge/corner resizing while anchoring every opposite edge. */
export function resizeCanvasRect(rect: LayoutRect, canvas: CanvasBounds, minimum: readonly [number, number], edge: ResizeEdge, dx: number, dy: number): LayoutRect {
  const [minWidth, minHeight] = minimum;
  let { x, y, width, height } = rect;
  if (edge.includes("w")) { x = clamped(snapped(rect.x + dx, canvas.snap), 0, rect.x + rect.width - minWidth); width = rect.x + rect.width - x; }
  if (edge.includes("e")) width = clamped(snapped(rect.width + dx, canvas.snap), minWidth, canvas.width - rect.x);
  if (edge.includes("n")) { y = clamped(snapped(rect.y + dy, canvas.snap), 0, rect.y + rect.height - minHeight); height = rect.y + rect.height - y; }
  if (edge.includes("s")) height = clamped(snapped(rect.height + dy, canvas.snap), minHeight, canvas.height - rect.y);
  return { x, y, width, height };
}

/** Resize selected edges while anchoring the opposite edges in canvas units. */
export function resizePanel(panel: PanelLayout, preset: LayoutPreset, edge: ResizeEdge, dx: number, dy: number): PanelLayout {
  if (![dx, dy].every(Number.isFinite)) return panel;
  return { ...panel, rect: resizeCanvasRect(panel.rect, preset, panelMinimum(panel.id), edge, dx, dy),
    z: Math.min(10000, Math.max(...preset.panels.map(item => item.z)) + 1) };
}

export function adjustPanel(panel: PanelLayout, preset: LayoutPreset, kind: "move" | "resize", dx: number, dy: number): PanelLayout {
  if (![dx, dy].every(Number.isFinite)) return panel;
  const rect = kind === "move" ? moveCanvasRect(panel.rect, preset, dx, dy)
    : resizeCanvasRect(panel.rect, preset, panelMinimum(panel.id), "se", dx, dy);
  return { ...panel, rect, z: Math.min(10000, Math.max(...preset.panels.map(item => item.z)) + 1) };
}

/** Trim unused height only: never move neighbours or create a new overlap. */
export function fitPanelHeight(panel: PanelLayout, preset: LayoutPreset, measuredHeight: number): PanelLayout {
  if (!Number.isFinite(measuredHeight) || measuredHeight <= 0) return panel;
  const step = preset.snap ? 8 : 1;
  const height = Math.min(panel.rect.height, Math.max(panelMinimum(panel.id)[1], Math.ceil(measuredHeight / step) * step));
  return { ...panel, rect: { ...panel.rect, height } };
}

export function customDeskPreset(base: LayoutPreset, height: number, equipment?: "rotor" | "antenna"): LayoutPreset {
  const preset = structuredClone(base);
  preset.id = "custom"; preset.name = "Custom"; preset.locked = false; preset.width = 1920; preset.height = Math.max(640, Math.min(2160, height));
  preset.panels.forEach(panel => { panel.placement = "hidden"; });
  const top = Math.min(340, preset.height - 300);
  const place = (id: PanelLayout["id"], rect: LayoutRect) => { const panel = preset.panels.find(item => item.id === id)!; panel.placement = "canvas"; panel.rect = rect; };
  place("active-qso", { x: 0, y: 0, width: equipment ? 980 : 1140, height: top });
  place("callsign-intelligence", { x: equipment ? 992 : 1152, y: 0, width: equipment ? 600 : 768, height: top });
  if (equipment) place(equipment, { x: 1604, y: 0, width: 316, height: top });
  place("world-map", { x: 0, y: top + 12, width: 1100, height: preset.height - top - 12 });
  place("band-maps", { x: 1112, y: top + 12, width: 808, height: preset.height - top - 12 });
  return preset;
}

/** Lay out complete rows in saved canvas coordinates. Taller desks scroll vertically
 * when panel minimum sizes cannot fit within one screen. */
export function tileDeskPanels(preset: LayoutPreset, viewportWidth: number, viewportHeight: number): LayoutPreset {
  const visible = preset.panels.filter(panel => panel.placement === "canvas");
  if (!visible.length) return preset;
  const width = Math.round(Math.min(16384, Math.max(640, viewportWidth || preset.width)));
  const availableHeight = Math.round(Math.min(16384, Math.max(480, viewportHeight || preset.height)));
  const gap = 10;
  const desired = Math.max(1, Math.round(Math.sqrt(visible.length * width / (availableHeight * 1.6))));
  const rows: PanelLayout[][] = [];
  let row: PanelLayout[] = [], minimumWidth = 0;
  for (const panel of visible) {
    const min = panelMinimum(panel.id)[0];
    if (row.length && (row.length >= desired || minimumWidth + gap + min > width)) {
      rows.push(row); row = []; minimumWidth = 0;
    }
    minimumWidth += min + (row.length ? gap : 0); row.push(panel);
  }
  rows.push(row);
  const heights = rows.map(items => Math.max(...items.map(panel => panel.id === "active-qso" ? 412 : panelMinimum(panel.id)[1])));
  const required = heights.reduce((sum, height) => sum + height, 0) + gap * (rows.length - 1);
  const height = Math.max(availableHeight, required);
  const extra = height - required;
  const rectangles = new Map<PanelLayout["id"], LayoutRect>();
  let y = 0;
  rows.forEach((items, index) => {
    const rowHeight = heights[index] + Math.floor(extra * (index + 1) / rows.length) - Math.floor(extra * index / rows.length);
    const free = width - gap * (items.length - 1) - items.reduce((sum, panel) => sum + panelMinimum(panel.id)[0], 0);
    let x = 0;
    items.forEach((panel, column) => {
      const panelWidth = panelMinimum(panel.id)[0] + Math.floor(free * (column + 1) / items.length) - Math.floor(free * column / items.length);
      rectangles.set(panel.id, { x, y, width: panelWidth, height: rowHeight }); x += panelWidth + gap;
    });
    y += rowHeight + gap;
  });
  return { ...preset, width, height, panels: preset.panels.map(panel => ({ ...panel, rect: rectangles.get(panel.id) ?? panel.rect })) };
}
