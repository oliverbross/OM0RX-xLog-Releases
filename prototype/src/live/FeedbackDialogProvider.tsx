import { lazy, Suspense, useEffect, useState, useSyncExternalStore, type ReactNode } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FeedbackContext } from "./ErrorNotice";
import { FeedbackStore } from "./feedbackStore";

const FeedbackDialog = lazy(() => import("./FeedbackDialog"));
export function FeedbackDialogProvider({ children, store: supplied }: { children: ReactNode; store?: FeedbackStore }) {
  const [store] = useState(() => supplied ?? new FeedbackStore());
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  useEffect(() => {
    if (!isTauri()) return;
    let active = true, unlisten: (() => void) | undefined;
    void listen("xlog:feedback-history", store.openHistory).then(stop => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, [store]);
  return <FeedbackContext.Provider value={store}>{children}
    {(snapshot.notices.length > 0 || snapshot.history) && <Suspense fallback={null}><FeedbackDialog store={store} /></Suspense>}
  </FeedbackContext.Provider>;
}
