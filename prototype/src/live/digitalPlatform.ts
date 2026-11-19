// WebView2 reports Win32 even for an x64 process. This controls presentation;
// the worker independently admits the native services on macOS and Windows.
export function digitalPlatformLimitation(): string | null {
  const platform = globalThis.navigator?.platform ?? "";
  const name = /Linux/i.test(platform) ? "Linux" : null;
  return name ? `WSJT-X and fldigi integration is unavailable on ${name}. You can import an ADIF file into the logbook.` : null;
}
