/** Admission contention is not a failed device operation or a lost workspace.
 * Keep these exact service responses distinct from unavailable/invalid data,
 * uncertain writes and any error with an operation receipt. */
export function isReadContention(cause: unknown): boolean {
  const text = (cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "").trim();
  return [
    "Prototype worker is busy; try again",
    "Station service is busy; try again.",
    "Radio session is busy; wait for its current action.",
    "Radio routing is unavailable. Radio session is busy; wait for its current action.",
  ].includes(text);
}
