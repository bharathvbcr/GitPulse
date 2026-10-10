import type { DiagnosticsStore } from "./diagnostics";

/** A navigation whose view settles this late is reported; the responsiveness probe's lag threshold. */
export const SLOW_NAVIGATION_MS = 250;
/**
 * DOM silence that counts as settled. A view whose data answers later than this
 * after its last change (a skeleton waiting on a slow IPC call) is timed to that
 * change, too early; the window does not add to the time reported.
 */
export const QUIET_MS = 500;
/** A navigation still changing the DOM after this long is reported as unsettled. */
export const SETTLE_LIMIT_MS = 10_000;
const REPORT_MS = 30_000;
/** Destinations remembered for cold/warm; the oldest is forgotten first. */
const MAX_DESTINATIONS = 512;
/** Durations kept per class for the session percentiles; the oldest are dropped. */
const MAX_SAMPLES = 512;

type Frame = (callback: () => void) => void;

export interface NavigationProbe {
  /**
   * The visible destination changed. `key` identifies it for cold/warm (it may
   * hold a repository path); `label` names it in the report and must not.
   */
  navigate(key: string, label: string): void;
  stop(): void;
}

interface Sample { label: string; cold: boolean; firstFrameMs: number; settledMs: number }

function percentile(sorted: number[], p: number): number {
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
}

function summary(name: string, values: number[]): string {
  if (values.length === 0) return `${name} n=0`;
  const sorted = [...values].sort((a, b) => a - b);
  const ms = (v: number) => Math.round(v);
  return `${name} n=${values.length} p50=${ms(percentile(sorted, 50))} p95=${ms(percentile(sorted, 95))} max=${ms(sorted[sorted.length - 1])} ms`;
}

/**
 * Times view and repository switches in the renderer, so cold and warm
 * navigation can be read from an installed build.
 *
 * A navigation runs from the store change that starts it to two moments: the
 * first frame after it (two animation frames, so the frame that renders the
 * change has been produced) and *settled*, the last DOM change before
 * {@link QUIET_MS} of silence. Settled includes data that arrives after the
 * first paint — a cold repository's hydrate, a lazy view's chunk — which is
 * what a person waits for, and also any background update landing in that
 * window, so it is an upper bound. Cold is the first visit to a destination
 * this session; warm is every later one.
 *
 * Normal navigation adds no diagnostic. A navigation that settles at or over
 * {@link SLOW_NAVIGATION_MS}, or does not settle within
 * {@link SETTLE_LIMIT_MS}, is reported — at most once per 30 s, with the
 * session's cold and warm distributions so one entry carries the context.
 * Navigations made while the document is hidden are not timed: a hidden
 * WKWebView defers frames, which is not a slow view.
 */
export function installNavigationDiagnostics(
  sink: Pick<DiagnosticsStore, "warn">,
  deps: {
    now?: () => number;
    frame?: Frame;
    /** Starts watching the DOM; returns the disconnect. */
    observe?: (onChange: () => void) => () => void;
    visible?: () => boolean;
  } = {},
): NavigationProbe {
  const now = deps.now ?? (() => performance.now());
  const frame: Frame = deps.frame ?? ((callback) => { requestAnimationFrame(() => callback()); });
  const observe = deps.observe ?? ((onChange) => {
    if (typeof MutationObserver === "undefined" || typeof document === "undefined") return () => {};
    const observer = new MutationObserver(onChange);
    observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true });
    return () => observer.disconnect();
  });
  const visible = deps.visible ?? (() => typeof document === "undefined" || document.visibilityState === "visible");

  const seen = new Set<string>();
  const cold: number[] = [];
  const warm: number[] = [];
  let slow: Sample[] = [];
  let unsettled = 0;
  let interrupted = 0;
  let lastReport = -Infinity;
  let reportTimer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;

  /** The navigation being timed; a newer one supersedes it. */
  let pending: {
    generation: number;
    label: string;
    cold: boolean;
    start: number;
    firstFrame: number | null;
    lastChange: number;
    quiet: ReturnType<typeof setTimeout> | null;
    limit: ReturnType<typeof setTimeout>;
    disconnect: () => void;
  } | null = null;
  let generation = 0;

  function keep(values: number[], value: number): void {
    values.push(value);
    if (values.length > MAX_SAMPLES) values.shift();
  }

  function end(): void {
    if (!pending) return;
    if (pending.quiet !== null) clearTimeout(pending.quiet);
    clearTimeout(pending.limit);
    pending.disconnect();
    pending = null;
  }

  function report(): void {
    if (reportTimer !== null) clearTimeout(reportTimer);
    reportTimer = null;
    if (stopped || (slow.length === 0 && unsettled === 0)) return;
    const at = now();
    if (at - lastReport < REPORT_MS) {
      reportTimer = setTimeout(report, lastReport + REPORT_MS - at);
      return;
    }
    const parts: string[] = [];
    if (slow.length > 0) {
      const worst = slow.reduce((a, b) => (b.settledMs > a.settledMs ? b : a));
      parts.push(
        `${slow.length} slow navigation(s) (settled >= ${SLOW_NAVIGATION_MS} ms); worst: ${worst.cold ? "cold" : "warm"} ${worst.label} settled_ms=${Math.round(worst.settledMs)} first_frame_ms=${Math.round(worst.firstFrameMs)}`,
      );
    }
    if (unsettled > 0) parts.push(`${unsettled} still changing after ${SETTLE_LIMIT_MS} ms`);
    parts.push(`session settled: ${summary("cold", cold)}, ${summary("warm", warm)}`);
    if (interrupted > 0) parts.push(`${interrupted} superseded before settling (not timed)`);
    sink.warn(
      "performance:nav",
      `${parts.join("; ")}. Settled is the last DOM change before ${QUIET_MS} ms without one, so background updates in that window count.`,
    );
    slow = [];
    unsettled = 0;
    interrupted = 0;
    lastReport = at;
  }

  function settle(): void {
    if (!pending || pending.firstFrame === null) return;
    const sample: Sample = {
      label: pending.label,
      cold: pending.cold,
      firstFrameMs: pending.firstFrame - pending.start,
      settledMs: Math.max(pending.lastChange, pending.firstFrame) - pending.start,
    };
    end();
    keep(sample.cold ? cold : warm, sample.settledMs);
    if (sample.settledMs >= SLOW_NAVIGATION_MS) {
      slow.push(sample);
      if (slow.length > MAX_SAMPLES) slow.shift();
      report();
    }
  }

  function quietFrom(at: number): void {
    if (!pending || pending.firstFrame === null) return;
    if (pending.quiet !== null) clearTimeout(pending.quiet);
    pending.quiet = setTimeout(settle, Math.max(0, at + QUIET_MS - now()));
  }

  return {
    navigate(key, label) {
      if (stopped) return;
      if (pending) {
        interrupted += 1;
        end();
      }
      const isCold = !seen.has(key);
      if (isCold) {
        if (seen.size >= MAX_DESTINATIONS) seen.delete(seen.values().next().value as string);
        seen.add(key);
      }
      if (!visible()) return;
      const mine = ++generation;
      const start = now();
      const disconnect = observe(() => {
        if (!pending || pending.generation !== mine) return;
        pending.lastChange = now();
        quietFrom(pending.lastChange);
      });
      pending = {
        generation: mine,
        label,
        cold: isCold,
        start,
        firstFrame: null,
        lastChange: start,
        quiet: null,
        limit: setTimeout(() => {
          if (!pending || pending.generation !== mine) return;
          end();
          unsettled += 1;
          report();
        }, SETTLE_LIMIT_MS),
        disconnect,
      };
      frame(() => frame(() => {
        if (!pending || pending.generation !== mine) return;
        pending.firstFrame = now();
        quietFrom(Math.max(pending.lastChange, pending.firstFrame));
      }));
    },
    stop() {
      stopped = true;
      end();
      if (reportTimer !== null) clearTimeout(reportTimer);
      reportTimer = null;
    },
  };
}
