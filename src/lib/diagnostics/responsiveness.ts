import type { DiagnosticsStore } from "./diagnostics";
import { noteEventLoopDelay, resetEventLoopDelay } from "../runtime/loadCadence";
import { activity as sharedActivity, type ActivityLog, type StallAttribution } from "./activity";

const INTERVAL_MS = 500;
const LAG_MS = 250;
const REPORT_MS = 30_000;
const SUSPEND_GAP_MS = 30_000;
/** Distinct causes tallied per report; the rest are counted together. */
const MAX_CAUSES = 8;

type VisibilityTarget = Pick<Document,
  "visibilityState" | "addEventListener" | "removeEventListener"
>;

type FocusTarget = Pick<EventTarget, "addEventListener" | "removeEventListener">;

/**
 * Low-frequency event-loop delay probe, including on WebKit where Long Tasks
 * entries are unavailable. Measures scheduling delay, not FPS. Each delayed
 * sample is attributed to what the renderer handled during the late interval
 * (an IPC answer, a watcher burst, or else the visible view) — see
 * ./activity.ts for why that is correlation rather than a profile.
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
    activity?: Pick<ActivityLog, "attribute">;
    /** Names the visible view; read only when a sample is late. */
    view?: () => string;
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
  const log = deps.activity ?? sharedActivity;
  const view = deps.view ?? (() => "unknown");
  let timer: ReturnType<typeof setTimeout> | null = null;
  let expected = 0;
  let armedAt = 0;
  let worst: StallAttribution | null = null;
  const causes = new Map<string, number>();
  let otherCauses = 0;
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
    worst = null;
    causes.clear();
    otherCauses = 0;
  }

  function attribute(at: number): StallAttribution {
    let name = "unknown";
    try { name = view() || "unknown"; } catch { /* a broken getter must not stop the probe */ }
    return log.attribute(armedAt, at, name);
  }

  function describe(a: StallAttribution): string {
    const commands = a.commands.map(([cmd, n]) => `${cmd}x${n}`);
    if (a.otherCommands > 0) commands.push(`+${a.otherCommands} more`);
    const partial = a.partial ? " (activity log wrapped; counts are lower bounds)" : "";
    return `cause=${a.cause} view=${a.view} commands=[${commands.join(",")}] watcher_events=${a.watcherEvents}${partial}`;
  }

  function stopTimer(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  }

  function report(at: number): void {
    if ((delayed === 0 && suspended === 0) || at - lastReport < REPORT_MS) return;
    const parts: string[] = [];
    if (delayed > 0) {
      parts.push(`${delayed} delayed UI timer sample(s); max_delay_ms=${Math.round(maxLag)}`);
      if (worst) parts.push(`worst: ${describe(worst)}`);
      const tally = [...causes].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
        .map(([cause, n]) => `${cause}x${n}`);
      if (otherCauses > 0) tally.push(`other x${otherCauses}`);
      parts.push(`causes: ${tally.join(", ")}`);
    }
    if (suspended > 0) parts.push(`${suspended} long observation gap(s); max_gap_ms=${Math.round(maxGap)} (may include system sleep or suspension)`);
    const note = delayed > 0
      ? "Causes are what the UI thread handled during each late interval, a correlation rather than a profile."
      : "This measures event-loop scheduling delay, not a specific cause.";
    sink.warn("performance:ui", `${parts.join("; ")}. ${note}`);
    discardSamples();
    lastReport = at;
  }

  function arm(): void {
    if (!observing()) return;
    armedAt = now();
    expected = armedAt + INTERVAL_MS;
    timer = setTimeout(tick, INTERVAL_MS);
  }

  function tick(): void {
    timer = null;
    if (!observing()) return;
    const at = now();
    const delay = at - expected;
    // The probe used to drop this number into a log. Schedulers read the
    // same sample so a late loop slows background work instead of only
    // being described after the fact.
    noteEventLoopDelay(delay);
    if (Number.isFinite(delay) && delay >= LAG_MS) {
      if (delay >= SUSPEND_GAP_MS) {
        suspended += 1;
        maxGap = Math.max(maxGap, delay);
      } else {
        delayed += 1;
        const attribution = attribute(at);
        if (delay >= maxLag) worst = attribution;
        maxLag = Math.max(maxLag, delay);
        if (causes.has(attribution.cause) || causes.size < MAX_CAUSES) {
          causes.set(attribution.cause, (causes.get(attribution.cause) ?? 0) + 1);
        } else {
          otherCauses += 1;
        }
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
      // flushing them on the next healthy tick after return. The load
      // sample is dropped with them: a hidden-window gap is not pressure
      // the next visible poll should inherit.
      discardSamples();
      resetEventLoopDelay();
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
