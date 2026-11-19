// Test-only helper: the Python interpreter for tests that run the real
// contest host. Not imported by application code.
import { spawnSync } from "node:child_process";

let found: string | undefined;

/** XLOG_CONTEST_PYTHON or XLOG_TEST_PYTHON when set; otherwise the first of
 *  the MacPorts path, python3.12, python3 or python that reports Python 3.12
 *  or newer. Windows installs usually provide only `python`. */
export function contestPython(): string {
  const explicit = process.env.XLOG_CONTEST_PYTHON ?? process.env.XLOG_TEST_PYTHON;
  if (explicit) return explicit;
  if (found) return found;
  for (const candidate of ["/opt/local/bin/python3.12", "python3.12", "python3", "python"]) {
    const result = spawnSync(candidate, ["--version"], { encoding: "utf8" });
    if (result.error || result.status !== 0) continue;
    const match = /Python (\d+)\.(\d+)/.exec(`${result.stdout}${result.stderr}`);
    if (match && Number(match[1]) === 3 && Number(match[2]) >= 12) return (found = candidate);
  }
  throw new Error("Real-host contest tests need Python 3.12 or newer; set XLOG_CONTEST_PYTHON to its path.");
}
