import type { DiagnosticsStore } from "./diagnostics";

const INTERVAL_MS = 500;
const LAG_MS = 250;
const REPORT_MS = 30_000;
const SUSPEND_GAP_MS = 30_000;

type VisibilityTarget = Pick<Document,
  "visibilityState" | "addEventListener" | "removeEventListener"
>;

/**
 * Low-frequency event-loop delay probe, including on WebKit where Long Tasks
 * entries are unavailable. Measures scheduling delay, not FPS or its cause.
 * Hidden windows stop the timer. Sleep-sized gaps are labelled as ambiguous.
 */
export function installResponsivenessDiagnostics(
  sink: Pick<DiagnosticsStore, "warn">,
  deps: { document?: VisibilityTarget | null; now?: () => number } = {},
): () => void {
  const target = deps.document !== undefined
    ? deps.document : typeof document === "undefined" ? null : document;
  if (!target) return () => {};
  const now = deps.now ?? (() => performance.now());
  let timer: ReturnType<typeof setTimeout> | null = null;
  let expected = 0;
  let lastReport = -Infinity;
  let delayed = 0;
  let maxLag = 0;
  let suspended = 0;
  let maxGap = 0;
  let stopped = false;

  function report(at: number): void {
    if ((delayed === 0 && suspended === 0) || at - lastReport < REPORT_MS) return;
    const parts: string[] = [];
    if (delayed > 0) parts.push(`${delayed} delayed UI timer sample(s); max_delay_ms=${Math.round(maxLag)}`);
    if (suspended > 0) parts.push(`${suspended} long observation gap(s); max_gap_ms=${Math.round(maxGap)} (may include system sleep or suspension)`);
    sink.warn("performance:ui", `${parts.join("; ")}. This measures event-loop scheduling delay, not a specific cause.`);
    delayed = 0;
    maxLag = 0;
    suspended = 0;
    maxGap = 0;
    lastReport = at;
  }

  function arm(): void {
    if (stopped || target?.visibilityState !== "visible") return;
    expected = now() + INTERVAL_MS;
    timer = setTimeout(tick, INTERVAL_MS);
  }

  function tick(): void {
    timer = null;
    if (stopped || target?.visibilityState !== "visible") return;
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

  function visibilityChanged(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    arm();
  }

  target.addEventListener("visibilitychange", visibilityChanged);
  arm();
  return () => {
    stopped = true;
    if (timer !== null) clearTimeout(timer);
    timer = null;
    target.removeEventListener("visibilitychange", visibilityChanged);
  };
}
