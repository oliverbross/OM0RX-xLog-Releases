import { useState } from "react";

type Group = { call: string; start: string; band: string; mode: string; contact_ids: string[] };
type Reply = { groups: Group[]; group_count: number; truncated: boolean };

/** Lists exact duplicate contacts (same call, start second, band and mode).
 *  Read-only: remove the extra copy with the logbook's own delete (with Undo). */
export function DuplicateContactsPanel({ onRequest }: { onRequest: (method: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  const [reply, setReply] = useState<Reply | null>(null), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const find = async () => {
    setBusy(true); setError("");
    try { setReply(await onRequest("logbook_duplicates", { limit: 200 }) as Reply); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "Duplicate contacts could not be listed."); }
    finally { setBusy(false); }
  };
  return <section className="duplicate-contacts" aria-label="Logbook duplicates">
    <h3>Logbook duplicates</h3>
    <p>Finds contacts stored more than once with the same call, start time, band and mode. Nothing is changed here; search the call in Logbook and delete the extra copy there (Undo is available).</p>
    <button type="button" disabled={busy} onClick={() => void find()}>{busy ? "Checking…" : "Find duplicate contacts"}</button>
    {error && <p role="alert">{error}</p>}
    {reply && (reply.groups.length === 0 ? <p role="status">No exact duplicate contacts.</p> : <>
      <p role="status">{reply.group_count}{reply.truncated ? "+" : ""} duplicate group{reply.group_count === 1 ? "" : "s"}:</p>
      <table><thead><tr><th>Call</th><th>UTC</th><th>Band</th><th>Mode</th><th>Copies</th></tr></thead>
        <tbody>{reply.groups.map(group => <tr key={group.contact_ids.join()}>
          <td>{group.call}</td><td>{group.start.replace("T", " ").replace("Z", "")}</td><td>{group.band.toLowerCase()}</td>
          <td>{group.mode}</td><td>{group.contact_ids.length}</td></tr>)}</tbody></table>
    </>)}
  </section>;
}

export default DuplicateContactsPanel;
