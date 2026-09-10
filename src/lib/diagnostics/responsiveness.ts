import type { DiagnosticsStore } from "./diagnostics";

const INTERVAL_MS = 500;
const LAG_MS = 250;
const REPORT_MS = 30_000;
const SUSPEND_GAP_MS = 30_000;

type VisibilityTarget = Pick<Document,
  "visibilityState" | "addEventListener" | "removeEventListener"
>;

type FocusTarget = Pick<EventTarget, "addEventListener" | "removeEventListener">;

/**
 * Low-frequency event-loop delay probe, including on WebKit where Long Tasks
 * entries are unavailable. Measures scheduling delay, not FPS or its cause.
 * Hidden or unfocused windows stop the timer and drop unreported samples —
 * WKWebView coalesces timers to ~1s behind another app while leaving
 * visibilityState "visible", which is not a UI freeze. Sleep-sized gaps are
 * labelled as ambiguous.
 */
export function installResponsivenessDiagnostics(
  sink: Pick<DiagnosticsStore, "warn">,
  deps: {
    document?: VisibilityTarget | null;
    window?: FocusTarget | null;
    now?: () => number;
    focused?: () => boolean;
  } = {},
): () => void {
  const injected = deps.document !== undefined;
  const target = injected
    ? deps.document : typeof document === "undefined" ? null : document;
  if (!target) return () => {};
  const win = injected
    ? (deps.window ?? null)
    : typeof window === "undefined" ? null : window;
  const now = deps.now ?? (() => performance.now());
  let timer: ReturnType<typeof setTimeout> | null = null;
  let expected = 0;
  let lastReport = -Infinity;
  let delayed = 0;
  let maxLag = 0;
  let suspended = 0;
  let maxGap = 0;
  let stopped = false;
  let focused = win === null
    ? true
    : (deps.focused?.() ?? (
      !injected && typeof document !== "undefined" && typeof document.hasFocus === "function"
        ? document.hasFocus()
        : true
    ));

  function observing(): boolean {
    return !stopped && target!.visibilityState === "visible" && focused;
  }

  function discardSamples(): void {
    delayed = 0;
    maxLag = 0;
    suspended = 0;
    maxGap = 0;
  }

  function stopTimer(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  }

  function report(at: number): void {
    if ((delayed === 0 && suspended === 0) || at - lastReport < REPORT_MS) return;
    const parts: string[] = [];
    if (delayed > 0) parts.push(`${delayed} delayed UI timer sample(s); max_delay_ms=${Math.round(maxLag)}`);
    if (suspended > 0) parts.push(`${suspended} long observation gap(s); max_gap_ms=${Math.round(maxGap)} (may include system sleep or suspension)`);
    sink.warn("performance:ui", `${parts.join("; ")}. This measures event-loop scheduling delay, not a specific cause.`);
    discardSamples();
    lastReport = at;
  }

  function arm(): void {
    if (!observing()) return;
    expected = now() + INTERVAL_MS;
    timer = setTimeout(tick, INTERVAL_MS);
  }

  function tick(): void {
    timer = null;
    if (!observing()) return;
    const at = now();
    const delay = at - expected;
    if (Number.isFinite(delay) && delay >= LAG_MS) {
      if (delay >= SUSPEND_GAP_MS) {
        suspended += 1;
        maxGap = Math.max(maxGap, delay);
      } else {
        delayed += 1;
        maxLag = Math.max(maxLag, delay);
      }
    }
    report(at);
    // Measure from AFTER recording: the observer's own work must not be
    // counted as another lag event on the next turn.
    arm();
  }

  function onActivityChanged(): void {
    stopTimer();
    if (!observing()) {
      // Samples collected while leaving the foreground include timer
      // coalescing, not a freeze the user can see. Drop them rather than
      // flushing them on the next healthy tick after return.
      discardSamples();
      return;
    }
    arm();
  }

  function onFocus(): void {
    focused = true;
    onActivityChanged();
  }

  function onBlur(): void {
    focused = false;
    onActivityChanged();
  }

  target.addEventListener("visibilitychange", onActivityChanged);
  win?.addEventListener("focus", onFocus);
  win?.addEventListener("blur", onBlur);
  arm();
  return () => {
    stopped = true;
    stopTimer();
    target.removeEventListener("visibilitychange", onActivityChanged);
    win?.removeEventListener("focus", onFocus);
    win?.removeEventListener("blur", onBlur);
  };
}
