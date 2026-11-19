import { ErrorNotice } from "./ErrorNotice";
import { useRef, useState } from "react";

type Projection = { flex_forwarding_available?: boolean; tci_forwarding_available?: boolean;
  snapshot?: { connectedClusters?: number; spots?: unknown[] }; forwarding?: { status?: string } };
export function PushToPanButton({ cluster, onRequest }: { cluster: unknown;
  onRequest?: (method: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  const projection = cluster as Projection | null;
  const pending = useRef(false);
  const [busy, setBusy] = useState(false), [message, setMessage] = useState(""), [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const available = projection?.flex_forwarding_available !== false || projection?.tci_forwarding_available === true;
  const push = async () => {
    if (!onRequest || pending.current) return;
    pending.current = true; setBusy(true); setMessage(""); setError("");
    setAttempt(value => value + 1);
    try {
      const result = await onRequest("cluster_push_spots", {}) as Projection;
      setMessage(result?.forwarding?.status || "Recent spots queued for the radio display.");
    } catch (cause) { setError(cause instanceof Error ? cause.message : "Could not push spots. Check the radio connection and try again."); }
    finally { pending.current = false; setBusy(false); }
  };
  return <>
    <button type="button" disabled={!onRequest || !available || busy || !projection?.snapshot?.connectedClusters || !projection.snapshot.spots?.length}
      title="Send filtered cluster spots from the past 30 minutes to the configured FLEX/Maestro/SmartSDR or TCI display"
      onClick={() => void push()}>{busy ? "Pushing…" : "Push to Pan"}</button>
    {(error || message) && <ErrorNotice when={true} title="Push to Pan" noticeKey={attempt}>
      <p role={error ? "alert" : "status"}>{error || (/failed/i.test(projection?.forwarding?.status ?? "") ? projection?.forwarding?.status : message)}</p>
    </ErrorNotice>}
  </>;
}
