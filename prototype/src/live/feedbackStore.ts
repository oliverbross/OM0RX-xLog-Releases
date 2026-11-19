export type FeedbackNotice = { source: string; key: string; title: string; text: string; historyOnly?: boolean; repeats?: number };

/** A notice that describes a momentary service condition (a busy or restarting
 *  station service, a read that did not answer). Such a notice waits this long
 *  before it opens the dialog; one that clears itself in time goes to History
 *  only, so a brief outage that recovers on its own interrupts nobody. */
export const transientNoticeGraceMs = 6_000;
const transientService = /\b(?:unavailable|not responding|did not respond|timed out|control outcome unknown|service request failed|worker generation changed|generation changed; no (?:request was sent|action was admitted)|service (?:stopped|closed)|is refreshing|try again shortly|shutting down)\b/i;
export function isTransientServiceNotice(text: string) { return transientService.test(text); }
type Snapshot = { notices: FeedbackNotice[]; history: boolean };

/** Presentation only: dismissing a notice never acknowledges an operation. */
export class FeedbackStore {
  private sources = new Map<string, FeedbackNotice>();
  private detached = new Map<string, FeedbackNotice>();
  private targets = new Map<string, HTMLElement>();
  private dismissed = new Set<string>();
  private pending = new Map<string, ReturnType<typeof setTimeout>>();
  /** How often each message has been raised anew (not re-rendered). */
  private counts = new Map<string, number>();
  private recent = new Map<string, FeedbackNotice>();
  private history = false;
  private snapshot: Snapshot = { notices: [], history: false };
  private listeners = new Set<() => void>();
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  getSnapshot = () => this.snapshot;
  getTarget = (source: string) => this.targets.get(source);
  isLive = (notice: FeedbackNotice) => this.sources.get(notice.source)?.key === notice.key;
  setTarget(source: string, target: HTMLElement | null) {
    if ((this.targets.get(source) ?? null) === target) return;
    if (target) this.targets.set(source, target); else this.targets.delete(source);
    this.publish();
  }
  private publish() {
    const unique = new Map<string, FeedbackNotice>();
    for (const notice of this.sources.values()) if (this.history || !notice.historyOnly && !this.dismissed.has(notice.key) && !this.pending.has(notice.source)) unique.set(notice.key, notice);
    for (const notice of this.detached.values()) if ((this.history || !notice.historyOnly) && !unique.has(notice.key)) unique.set(notice.key, notice);
    if (this.history) for (const notice of this.recent.values()) if (!unique.has(notice.key)) unique.set(notice.key, notice);
    this.snapshot = { notices: [...unique.values()].map(notice => {
      const repeats = this.counts.get(notice.key) ?? 1;
      return repeats > 1 ? { ...notice, repeats } : notice;
    }), history: this.history };
    this.listeners.forEach(listener => listener());
  }
  report(notice: FeedbackNotice) {
    const previous = this.sources.get(notice.source);
    const same = previous?.key === notice.key;
    const shown = same && !this.pending.has(notice.source);
    if (!same) {
      this.counts.set(notice.key, (this.counts.get(notice.key) ?? 0) + 1);
      while (this.counts.size > 512) this.counts.delete(this.counts.keys().next().value!);
    }
    if (same) { /* a re-render re-reports the same notice: keep its state and timer */ }
    else if (!notice.historyOnly && isTransientServiceNotice(notice.text) && !this.detached.has(notice.key)) {
      clearTimeout(this.pending.get(notice.source));
      this.pending.set(notice.source, setTimeout(() => { this.pending.delete(notice.source); this.publish(); }, transientNoticeGraceMs));
    } else {
      clearTimeout(this.pending.get(notice.source)); this.pending.delete(notice.source);
    }
    void shown;
    this.sources.set(notice.source, notice);
    this.detached.delete(notice.key);
    this.recent.delete(notice.key);
    this.recent.set(notice.key, notice);
    // Retain a bounded text-only history, never callbacks from unmounted views.
    while (this.recent.size > 50) this.recent.delete(this.recent.keys().next().value!);
    this.publish();
  }
  withdraw(source: string) {
    const notice = this.sources.get(source);
    if (!notice) return;
    this.sources.delete(source);
    // A momentary condition that cleared within its grace period never opened
    // the dialog; it remains in History only.
    if (this.pending.has(source)) {
      clearTimeout(this.pending.get(source)); this.pending.delete(source);
      this.publish();
      return;
    }
    // A toast timer or background recovery must not close a dialog while it is
    // being read. Drop callbacks from a retired view and retain its text only.
    if (!this.dismissed.has(notice.key)) this.detached.set(notice.key, notice);
    while (this.detached.size > 50) this.detached.delete(this.detached.keys().next().value!);
    this.publish();
  }
  dismiss = () => {
    for (const notice of [...this.sources.values(), ...this.detached.values()]) this.counts.delete(notice.key);
    for (const notice of this.sources.values()) this.dismissed.add(notice.key);
    for (const notice of this.detached.values()) this.dismissed.add(notice.key);
    this.detached.clear();
    while (this.dismissed.size > 512) this.dismissed.delete(this.dismissed.values().next().value!);
    this.history = false;
    this.publish();
  };
  openHistory = () => { this.history = true; this.publish(); };
}
