/** Retry only native pre-dispatch admission refusal. A timeout or uncertain
 * command outcome must never be replayed here, including read-like commands. */
export async function waitForWorkerAdmission<T>(send: () => Promise<T>): Promise<T> {
  const delays = [100, 250, 500, 1_000, 2_000, 4_000];
  for (let attempt = 0; ; attempt++) {
    try { return await send(); }
    catch (cause) {
      const message = cause instanceof Error ? cause.message : cause;
      if (message !== "Prototype worker is busy; try again" || attempt >= delays.length) throw cause;
      await new Promise(resolve => setTimeout(resolve, delays[attempt]));
    }
  }
}
