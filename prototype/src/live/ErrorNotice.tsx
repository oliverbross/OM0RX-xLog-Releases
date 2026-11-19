import { createContext, isValidElement, useContext, useEffect, useId, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { FeedbackStore } from "./feedbackStore";
import { isReadContention } from "./readContention";

export const FeedbackContext = createContext<FeedbackStore | null>(null);
export const FeedbackContentContext = createContext(false);
const noSubscription = () => () => {};
const emptySnapshot = { notices: [], history: false };
const readEmpty = () => emptySnapshot;

export function feedbackText(node: ReactNode, includeActions = true): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(child => feedbackText(child, includeActions)).join(" ");
  if (isValidElement<{ children?: ReactNode; "aria-hidden"?: boolean; role?: string }>(node) && !node.props["aria-hidden"]
    && (includeActions || node.type !== "button" && node.props.role !== "button")) return feedbackText(node.props.children, includeActions);
  return "";
}

function hasComponentContent(node: ReactNode): boolean {
  if (Array.isArray(node)) return node.some(hasComponentContent);
  if (!isValidElement<{ children?: ReactNode }>(node)) return false;
  return typeof node.type === "function" && node.props.children === undefined || hasComponentContent(node.props.children);
}

/** Route existing error content and its recovery actions out of the working layout. */
export function ErrorNotice({ children, when, title = "Something went wrong", noticeKey = "" }: {
  children: ReactNode; when?: boolean; title?: string; noticeKey?: string | number;
}) {
  const store = useContext(FeedbackContext), inside = useContext(FeedbackContentContext), source = useId();
  const snapshot = useSyncExternalStore(store && !inside ? store.subscribe : noSubscription, store && !inside ? store.getSnapshot : readEmpty);
  const props = isValidElement<{ role?: string; className?: string; hidden?: boolean }>(children) ? children.props : {};
  const text = feedbackText(children).replace(/\s+/g, " ").trim() || (hasComponentContent(children) ? title : "");
  // Only a real failure interrupts with the dialog: an alert, or a caller that
  // says so with `when`. Status lines (background state, progress, receipts,
  // history) and text that is merely styled as an error stay inline beside
  // the control they describe, so routine activity never pops a dialog.
  const active = !props.hidden && (when ?? props.role === "alert");
  const key = `${title}\n${noticeKey === "" ? text : noticeKey}`;
  const historyOnly = isReadContention(feedbackText(children, false).replace(/\s+/g, " ").trim());
  useEffect(() => {
    if (!store || inside) return;
    if (active && text) store.report({ source, key, title, text, historyOnly }); else store.withdraw(source);
  }, [store, inside, active, text, key, title, children, source, historyOnly]);
  useEffect(() => () => { if (!inside) store?.withdraw(source); }, [store, inside, source]);
  // Standalone embedders retain semantic feedback; every application window
  // installs the shared provider at its bootstrap boundary.
  if (!store || inside || !active) return children;
  const target = store.getTarget(source);
  // Render from the originating component so local form/device contexts and
  // current recovery callbacks survive the move into the shared dialog.
  return target && snapshot.notices.some(notice => notice.source === source)
    ? createPortal(<FeedbackContentContext.Provider value={true}>{children}</FeedbackContentContext.Provider>, target) : null;
}
