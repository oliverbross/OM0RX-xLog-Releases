import { ErrorNotice } from "./ErrorNotice";
import { activationMessage } from "./spotActivationClient";
import type { SpotActivationSnapshot } from "./spotActivation";
export function SpotActivationStatus({ status, error, inline = false }: {
  status: SpotActivationSnapshot | null; error?: string; checking?: boolean; reconcile?: () => void; retire?: () => void; inline?: boolean;
}) {
  const progress = status && ["preparing", "draft_applied", "tuning"].includes(status.phase);
  if (inline) return progress ? <ErrorNotice><span className="secondary" role="status">Activating report…</span></ErrorNotice> : null;
  if (progress) return error ? <ErrorNotice><div className="live-cluster-note" role="alert"><ErrorNotice when={true}><span>{error}</span></ErrorNotice></div></ErrorNotice> : null;
  if (status?.phase === "uncertain") return null;
  if (status?.phase === "confirmed" && !error && !status.error && status.reservationActive !== true && !/uncertain|failed|rejected|error/i.test(status.rotor || "")) return null;
  const text = error || activationMessage(status);
  if (!text) return null;
  return <ErrorNotice><div className="live-cluster-note" role={error ? "alert" : "status"}><span>{text}</span></div></ErrorNotice>;
}
