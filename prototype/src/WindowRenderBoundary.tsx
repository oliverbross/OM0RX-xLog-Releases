import { ErrorNotice } from "./live/ErrorNotice";
import { Component, type ReactNode } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Close through the window's own lifecycle: CW goes through its owner, which
 *  stops any keying before the window is destroyed. */
async function closeFailedWindow() {
  const window = getCurrentWindow();
  if (window.label === "cw") await invoke("close_cw_window");
  else await window.close();
}

/** A failed surface stays failed until its native window is closed normally.
 * Do not remount, reload, or bypass the existing draft/output close lifecycle. */
export class WindowRenderBoundary extends Component<{ children: ReactNode }, { failed: boolean; closeError: string }> {
  state = { failed: false, closeError: "" };

  static getDerivedStateFromError() { return { failed: true }; }

  render() {
    if (!this.state.failed) return this.props.children;
    return <div className="app live monitor-app">
      <main aria-label="Window display error">
        <ErrorNotice><div className="live-error" role="alert">
          <div>
            <h1>This window could not continue.</h1>
            <p>A display error interrupted the interface.</p>
            <p>Close the window, then reopen it from the Window menu. After reopening, review any unfinished QSO edits before continuing.</p>
            <p>Error code: UI_RENDER_FAILED</p>
            {isTauri() && <button type="button" onClick={() => { this.setState({ closeError: "" }); void closeFailedWindow().catch(cause => this.setState({ closeError: String(cause) })); }}>Close window</button>}
            {this.state.closeError && <p>{this.state.closeError}</p>}
          </div>
        </div></ErrorNotice>
      </main>
    </div>;
  }
}
