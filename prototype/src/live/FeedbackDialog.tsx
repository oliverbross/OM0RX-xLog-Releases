import { useCallback, useSyncExternalStore } from "react";
import { AppDialog } from "./AppDialog";
import type { FeedbackStore, FeedbackNotice } from "./feedbackStore";
import "./FeedbackDialog.css";

function FeedbackSlot({ store, notice }: { store: FeedbackStore; notice: FeedbackNotice }) {
  const ref = useCallback((target: HTMLDivElement | null) => store.setTarget(notice.source, target), [store, notice.source]);
  const repeats = notice.repeats && notice.repeats > 1 ? <p className="feedback-repeats">Seen {notice.repeats} times</p> : null;
  return <>{store.isLive(notice) ? <div className="feedback-message" ref={ref} /> : <p className="feedback-message">{notice.text}</p>}{repeats}</>;
}

export default function FeedbackDialog({ store }: { store: FeedbackStore }) {
  const { notices, history } = useSyncExternalStore(store.subscribe, store.getSnapshot);
  if (!notices.length && !history) return null;
  return <AppDialog title={history ? "Recent messages" : notices[0].title} role="alertdialog" className="feedback-dialog" onClose={store.dismiss}>
      {notices[0] ? <FeedbackSlot store={store} notice={notices[0]} /> : <p>No recent messages.</p>}
      <details className="feedback-additional" hidden={notices.length < 2}>
        <summary>{notices.length - 1} more message{notices.length === 2 ? "" : "s"}</summary>
        {notices.slice(1).map(notice => <FeedbackSlot key={notice.key} store={store} notice={notice} />)}
      </details>
    <div className="dialog-actions"><button type="button" data-dialog-autofocus onClick={store.dismiss}>Close</button></div>
  </AppDialog>;
}
