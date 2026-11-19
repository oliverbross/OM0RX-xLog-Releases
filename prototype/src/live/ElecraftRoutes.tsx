import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { ask, bands, type Route } from "./elecraftClient";

export default function ElecraftRoutes({
  names,
  port,
}: {
  names: string[];
  port: string | null;
}) {
  const [rows, setRows] = useState<Record<string, Route>>({});
  const [edits, setEdits] = useState<Record<string, Route>>({});
  const [error, setError] = useState("");
  const [receipt, setReceipt] = useState("");
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let current = true;
    setRows({});
    setEdits({});
    setError("");
    setReceipt("");
    setBusy(!!port);
    async function read() {
      try {
        for (const band of bands) {
          if (!current) return;
          const route = await ask<Route>("route", "kat", { band });
          if (current) setRows((old) => ({ ...old, [band]: route }));
        }
      } catch (e) {
        if (current) setError(String(e));
      } finally {
        if (current) setBusy(false);
      }
    }
    if (port) void read();
    return () => {
      current = false;
    };
  }, [port, revision]);
  const change = (band: string, route: Route) => {
    setReceipt("");
    setEdits((old) => {
      const next = { ...old, [band]: route };
      if (
        route.preferred === rows[band]?.preferred &&
        route.enabled.every((v, i) => v === rows[band].enabled[i])
      )
        delete next[band];
      return next;
    });
  };
  async function save() {
    setBusy(true);
    setError("");
    setReceipt("");
    let count = 0;
    try {
      for (const [band, route] of Object.entries(edits)) {
        const confirmed = await ask<Route>("save_route", "kat", {
          band,
          route,
        });
        setRows((old) => ({ ...old, [band]: confirmed }));
        setEdits((old) => {
          const next = { ...old };
          delete next[band];
          return next;
        });
        count++;
        setReceipt(
          count +
            " band assignment" +
            (count === 1 ? "" : "s") +
            " saved and verified.",
        );
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <fieldset>
      <legend>Band and antenna assignments</legend>
      <p>
        {port
          ? "Choose a preferred antenna for each band. Enabled outputs are available for manual selection."
          : "Connect the KAT500 to read and edit its antenna assignments."}
      </p>
      {port && (
        <>
          <div className="ec-table-wrap">
            <table className="ec-route-table">
              <thead>
                <tr>
                  <th>Band</th>
                  <th>Preferred output</th>
                  {names.map((n, i) => (
                    <th key={i}>
                      ANT {i + 1} · {n}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {bands.map((band) => {
                  const row = edits[band] ?? rows[band];
                  return (
                    <tr key={band}>
                      <th scope="row">{band}</th>
                      <td>
                        {row ? (
                          <select
                            aria-label={band + " preferred output"}
                            disabled={busy}
                            value={row.preferred}
                            onChange={(e) => {
                              const preferred = Number(e.target.value);
                              change(band, {
                                preferred,
                                enabled: row.enabled.map(
                                  (v, i) => v || i + 1 === preferred,
                                ),
                              });
                            }}
                          >
                            <option value={0}>Last used</option>
                            {names.map((n, i) => (
                              <option value={i + 1} key={i}>
                                ANT {i + 1} · {n}
                              </option>
                            ))}
                          </select>
                        ) : (
                          "—"
                        )}
                      </td>
                      {names.map((n, i) => (
                        <td key={i}>
                          {row && (
                            <input
                              type="checkbox"
                              aria-label={
                                band + " enable ANT " + (i + 1) + " " + n
                              }
                              checked={row.enabled[i]}
                              disabled={
                                busy ||
                                row.preferred === i + 1 ||
                                (row.enabled[i] &&
                                  row.enabled.filter(Boolean).length === 1)
                              }
                              onChange={(e) =>
                                change(band, {
                                  ...row,
                                  enabled: row.enabled.map((v, j) =>
                                    i === j ? e.target.checked : v,
                                  ),
                                })
                              }
                            />
                          )}
                        </td>
                      ))}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div className="ec-actions">
            <button
              type="button"
              onClick={() => void save()}
              disabled={busy || !Object.keys(edits).length}
            >
              Save antenna assignments
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => setRevision((v) => v + 1)}
            >
              {Object.keys(edits).length
                ? "Discard changes and read tuner"
                : "Read tuner"}
            </button>
          </div>
        </>
      )}
      <ErrorNotice when={Boolean(error) || undefined}><output role="status">
        {error || receipt || (busy ? "Working…" : "")}
      </output></ErrorNotice>
    </fieldset>
  );
}
