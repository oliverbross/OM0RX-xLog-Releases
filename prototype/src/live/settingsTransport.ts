import { workerRequest } from "../worker";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTransientStartupReadFailure, serviceErrorMessage } from "./startupReadRecovery";

let settingsRead: Promise<unknown> | undefined;
export async function requestSettings(method: string, params: Record<string, unknown> = {}) {
  if (method === "settings_load" && !Object.keys(params).length) {
    if (!settingsRead) settingsRead = workerRequest(method, params).then(reply => reply.result)
      .finally(() => { settingsRead = undefined; });
    return settingsRead;
  }
  return (await workerRequest(method, params)).result;
}

/** Wake-only invalidation. Every surface reloads and validates the authoritative
 * snapshot; no settings or command authority travels in the event payload. */
export function connectSettingsChanges(changed: () => void | Promise<void>, failed: (message: string) => void) {
  if (!isTauri()) return () => {};
  let active = true, timer: ReturnType<typeof setTimeout> | undefined, stop: (() => void) | undefined;
  let running = false, queued = false, retryDelay = 1_000;
  const reload = async () => {
    if (!active) return;
    if (running) { queued = true; return; }
    running = true; queued = false;
    let retry = false;
    try { await changed(); retryDelay = 1_000; }
    catch (cause) {
      // Retain the current snapshot and unsaved editor while a background read
      // waits for admission. This is not a failed save and needs no global toast.
      if (isTransientStartupReadFailure(cause)) retry = true;
      else if (active) failed(serviceErrorMessage(cause) || "Settings could not be reloaded.");
    } finally {
      running = false;
      if (active && (retry || queued)) {
        clearTimeout(timer);
        timer = setTimeout(() => { timer = undefined; void reload(); }, retry ? retryDelay : 50);
        if (retry) retryDelay = Math.min(30_000, retryDelay * 2);
      }
    }
  };
  const schedule = () => {
    if (!active) return;
    if (running) { queued = true; return; }
    clearTimeout(timer);
    timer = setTimeout(() => { timer = undefined; void reload(); }, 50);
  };
  window.addEventListener("focus", schedule);
  void listen("xlog:settings-changed", schedule).then(dispose => {
    if (!active) { dispose(); return; }
    stop = dispose; schedule();
  }).catch(() => { if (active) failed("Live Settings updates are unavailable. Refocus this window to reload."); });
  return () => { active = false; clearTimeout(timer); stop?.(); window.removeEventListener("focus", schedule); };
}
