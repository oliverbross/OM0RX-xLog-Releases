import { useEffect, useRef, useState, type MouseEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { ChaseTargetDialog } from "./ChaseTargetDialog";

type Target = { callsign: string; band?: string; mode?: string };
export function useClusterAlertMenu(enabled = true) {
  const [menu, setMenu] = useState<{ target: Target; x: number; y: number } | null>(null);
  const [target, setTarget] = useState<Target | null>(null);
  const root = useRef<HTMLDivElement>(null), origin = useRef<HTMLElement | null>(null);
  const close = () => { setMenu(null); origin.current?.focus(); };
  useEffect(() => {
    if (!menu) return;
    root.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const outside = (event: PointerEvent) => { if (!root.current?.contains(event.target as Node)) setMenu(null); };
    const key = (event: globalThis.KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); close(); } else if (event.key === "Tab") setMenu(null); };
    window.addEventListener("pointerdown", outside); window.addEventListener("keydown", key);
    return () => { window.removeEventListener("pointerdown", outside); window.removeEventListener("keydown", key); };
  }, [menu]);
  const open = (event: MouseEvent<HTMLElement> | KeyboardEvent<HTMLElement>, spot: Target) => {
    if (!enabled) return;
    event.preventDefault(); event.stopPropagation();
    origin.current = event.currentTarget.closest<HTMLElement>("tr") ?? event.currentTarget;
    const box = event.currentTarget.getBoundingClientRect();
    const x = "clientX" in event ? event.clientX : box.left, y = "clientY" in event ? event.clientY : box.bottom;
    setMenu({ target: { callsign: spot.callsign, band: spot.band, mode: spot.mode }, x: Math.max(8, Math.min(x, window.innerWidth - 188)), y: Math.max(8, Math.min(y, window.innerHeight - 52)) });
  };
  return { open, onKeyDown: (event: KeyboardEvent<HTMLElement>, spot: Target) => {
    if (event.key === "ContextMenu" || event.shiftKey && event.key === "F10") { open(event, spot); return enabled; }
    return false;
  }, overlay: <>
    {menu && createPortal(<div ref={root} className="cluster-call-menu" role="menu" aria-label="Callsign actions" style={{ left: menu.x, top: menu.y }}>
      <button type="button" role="menuitem" onClick={() => { setTarget(menu.target); setMenu(null); }}>Add to Alerts</button>
    </div>, document.body)}
    {target && <ChaseTargetDialog {...target} onClose={() => { setTarget(null); origin.current?.focus(); }} />}
  </> };
}
