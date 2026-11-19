import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

/** Secondary macro tools open over the desk; they never resize the QSO panel. */
export default function MacroWorkbench({ title, children, events = [] }: { title: string; children: ReactNode; events?: string[] }) {
  const [open, setOpen] = useState(false), dialog = useRef<HTMLDialogElement>(null);
  const focusEntry = useRef(false);
  useEffect(() => {
    const show = () => { focusEntry.current = true; setOpen(true); };
    events.forEach(event => window.addEventListener(event, show));
    return () => events.forEach(event => window.removeEventListener(event, show));
  }, [events.join("|")]);
  useEffect(() => { if (open && !dialog.current?.open) dialog.current?.showModal?.(); if (open && focusEntry.current) { dialog.current?.querySelector("textarea")?.focus(); focusEntry.current = false; } }, [open]);
  return <><button type="button" onClick={() => setOpen(true)}>{title}</button>{createPortal(
    <dialog ref={dialog} className="macro-workbench" aria-label={title} onClose={() => setOpen(false)}>
      <header><strong>{title}</strong><button type="button" onClick={() => { dialog.current?.close(); setOpen(false); }}>Close</button></header>
      {children}
    </dialog>, document.body)}</>;
}
