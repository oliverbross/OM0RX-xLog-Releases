import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import "./AppDialog.css";
export function AppDialog({ title, children, onClose, closeDisabled = false, className = "", role = "dialog" }: { title: string; children: ReactNode; onClose: () => void; closeDisabled?: boolean; className?: string; role?: "dialog" | "alertdialog" }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const dialog = ref.current;
    if (dialog?.showModal) dialog.showModal(); else dialog?.setAttribute("open", "");
    dialog?.querySelector<HTMLElement>("[data-dialog-autofocus]")?.focus({ preventScroll: true });
    return () => { dialog?.close?.(); previous?.focus({ preventScroll: true }); };
  }, []);
  return createPortal(<dialog ref={ref} role={role} className={`xlog-dialog ${className}`.trim()} aria-label={title} onCancel={event => { event.preventDefault(); if (!closeDisabled) onClose(); }}>
    <header><h2>{title}</h2><button type="button" aria-label="Close dialog" disabled={closeDisabled} onClick={onClose}>×</button></header>
    <div className="xlog-dialog-content">{children}</div>
  </dialog>, document.body);
}
