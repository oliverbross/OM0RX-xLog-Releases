import { ErrorNotice } from "./ErrorNotice";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

type Listener = { host: string; port: number; revision: string };

export default function AgentLocalApi({ revision = 0 }: { revision?: number }) {
  const [saved, setSaved] = useState<Listener | null>(null);
  const [host, setHost] = useState("127.0.0.1"),
    [port, setPort] = useState("8840");
  const [token, setToken] = useState<string | null>(null),
    [show, setShow] = useState(false);
  const [message, setMessage] = useState(""),
    [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    invoke<Listener>("elecraft_agent", { action: "api_settings" })
      .then((v) => {
        if (active) {
          setSaved(v);
          setHost(v.host);
          setPort(String(v.port));
          setToken(null);
          setShow(false);
          setMessage("");
        }
      })
      .catch((e) => active && setMessage(String(e)));
    return () => {
      active = false;
    };
  }, [revision]);
  const actualToken = async () =>
    token ?? (await invoke<string>("elecraft_agent", { action: "api_token" }));
  async function save() {
    if (!saved) return;
    setBusy(true);
    try {
      const next = await invoke<Listener>("elecraft_agent", {
        action: "save_api",
        config: {
          host,
          port: Number(port),
          revision: saved.revision,
          ...(token === null ? {} : { token }),
        },
      });
      setSaved(next);
      setToken(null);
      setShow(false);
      setMessage("API settings saved. Restart the Agent to apply them.");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function restart() {
    setBusy(true);
    try {
      await invoke("elecraft_agent", { action: "restart" });
      setMessage("Agent restart requested.");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <fieldset disabled={busy}>
      <legend>Local API and security</legend>
      <div className="ec-settings">
        <label>
          Bind address
          <select value={host} onChange={(e) => setHost(e.target.value)}>
            {["127.0.0.1", "localhost", "::1"].map((v) => (
              <option key={v}>{v}</option>
            ))}
          </select>
        </label>
        <label>
          API port
          <input
            type="number"
            min={1}
            max={65535}
            value={port}
            onChange={(e) => setPort(e.target.value)}
          />
        </label>
        <label>
          Bearer token
          <input
            autoComplete="off"
            spellCheck={false}
            type={show ? "text" : "password"}
            placeholder="Stored token"
            value={token ?? ""}
            onChange={(e) => setToken(e.target.value)}
          />
        </label>
      </div>
      <div className="ec-actions">
        <button
          type="button"
          onClick={() => {
            if (show) {
              setShow(false);
              return;
            }
            void actualToken()
              .then((t) => {
                setToken(t);
                setShow(true);
              })
              .catch((e) => setMessage(String(e)));
          }}
        >
          {show ? "Hide" : "Show"} token
        </button>
        <button
          type="button"
          onClick={() =>
            void actualToken()
              .then((t) => navigator.clipboard.writeText(t))
              .then(() => setMessage("Token copied."))
              .catch((e) => setMessage(String(e)))
          }
        >
          Copy token
        </button>
        <button
          type="button"
          onClick={() => {
            const bytes = crypto.getRandomValues(new Uint8Array(32));
            setToken(
              Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(
                "",
              ),
            );
            setMessage("New token generated. Save API settings to use it.");
          }}
        >
          Generate new token
        </button>
        <button type="button" disabled={!saved} onClick={() => void save()}>
          Save API settings
        </button>
        <button type="button" onClick={() => void restart()}>
          Restart Station Agent
        </button>
      </div>
      <ErrorNotice when={true} title="Message"><output role="status">{message}</output></ErrorNotice>
    </fieldset>
  );
}
