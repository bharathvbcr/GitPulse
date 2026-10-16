/**
 * Frame pacing as the page sees it: the interval between successive
 * `requestAnimationFrame` callbacks while a probe runs.
 *
 * `responsiveness.ts` measures event-loop stalls at 500 ms resolution, which
 * says nothing about frames. The renderer A/Bs (ft-406d1f4a, ft-cc4be67b) need
 * the rAF frame-interval p95 and the fraction of dropped frames — intervals
 * longer than 1.5x the frame period — taken inside the real app, so this
 * probe is driven from a terminal: a program writes
 *
 *   OSC 7700 ; frames-start [; period=<ms>] BEL   start recording
 *   OSC 7700 ; frames-stop BEL                    stop, and answer
 *
 * and the answer is written back to the PTY as the terminal's own reply,
 * the way a cursor-position report is:
 *
 *   OSC 7700 ; frames ; n=<intervals> ; dropped=<n> ; fraction=<f> ;
 *     period=<ms> ; p50=<ms> ; p95=<ms> ; ms=<recorded> BEL
 *
 * `period` is what "dropped" is measured against. WebKit may run rAF slower
 * than the display (60 Hz on a 120 Hz panel), so the caller measures it with
 * an idle probe first and passes it in; without one, the run's own median is
 * used, which a loaded run would inflate.
 *
 * What a program in the terminal can learn this way is the page's frame
 * timing, nothing else: no content, no other session's data, no input. A
 * probe stops itself after `MAX_PROBE_MS` and keeps at most one interval per
 * frame of that window.
 */

/** The private OSC the probe answers to. */
export const FRAME_PROBE_OSC = 7700;
/** A probe nobody stops ends here; its intervals are kept for the stop. */
export const MAX_PROBE_MS = 120_000;
/** Longer than this multiple of the period counts as a dropped frame. */
const DROPPED_FACTOR = 1.5;
/** Ceiling on recorded intervals: 120 s at 240 Hz. */
const MAX_INTERVALS = 28_800;

export interface FrameStats {
  intervals: number;
  dropped: number;
  fraction: number;
  periodMs: number;
  p50Ms: number;
  p95Ms: number;
  recordedMs: number;
}

function quantile(sorted: number[], q: number): number {
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
}

/** Summary of recorded intervals; null when fewer than two frames ran. */
export function frameStats(intervals: number[], periodMs?: number): FrameStats | null {
  if (!intervals.length) return null;
  const sorted = [...intervals].sort((a, b) => a - b);
  const p50 = quantile(sorted, 0.5);
  const period = periodMs !== undefined && Number.isFinite(periodMs) && periodMs > 0 ? periodMs : p50;
  const dropped = intervals.filter((interval) => interval > DROPPED_FACTOR * period).length;
  return {
    intervals: intervals.length,
    dropped,
    fraction: dropped / intervals.length,
    periodMs: period,
    p50Ms: p50,
    p95Ms: quantile(sorted, 0.95),
    recordedMs: intervals.reduce((sum, interval) => sum + interval, 0),
  };
}

export interface FrameProbe {
  start(periodMs?: number): void;
  /** Stops (if running) and summarises what was recorded since the start. */
  stop(): FrameStats | null;
  running(): boolean;
}

export function createFrameProbe(deps: {
  raf?: (callback: (time: number) => void) => number;
  cancel?: (handle: number) => void;
  maxMs?: number;
} = {}): FrameProbe {
  const raf = deps.raf ?? ((callback) => requestAnimationFrame(callback));
  const cancel = deps.cancel ?? ((handle) => cancelAnimationFrame(handle));
  const maxMs = deps.maxMs ?? MAX_PROBE_MS;
  let handle: number | null = null;
  let intervals: number[] = [];
  let period: number | undefined;
  let last: number | null = null;
  let first: number | null = null;

  const tick = (time: number) => {
    handle = null;
    if (last !== null) intervals.push(time - last);
    first ??= time;
    last = time;
    if (time - first >= maxMs || intervals.length >= MAX_INTERVALS) return;
    handle = raf(tick);
  };

  const halt = () => {
    if (handle !== null) cancel(handle);
    handle = null;
  };

  return {
    start(periodMs) {
      halt();
      intervals = [];
      period = periodMs;
      last = null;
      first = null;
      handle = raf(tick);
    },
    stop() {
      halt();
      return frameStats(intervals, period);
    },
    running: () => handle !== null,
  };
}

/** Rounds for the reply: milliseconds to 0.01, fractions to 1e-4. */
const round = (value: number, places: number) => Number(value.toFixed(places));

/**
 * One OSC 7700 payload (what follows `7700;`), applied to `probe`. Returns
 * the reply to write to the PTY, or null when the payload asks for none.
 * Unknown payloads are ignored rather than echoed.
 */
export function handleFrameProbeCommand(probe: FrameProbe, payload: string): string | null {
  const [command, ...fields] = payload.split(";");
  if (command === "frames-start") {
    const given = fields.find((field) => field.startsWith("period="));
    const period = given ? Number(given.slice("period=".length)) : undefined;
    probe.start(period !== undefined && Number.isFinite(period) && period > 0 ? period : undefined);
    return null;
  }
  if (command !== "frames-stop") return null;
  const stats = probe.stop();
  const body = stats
    ? [
      `n=${stats.intervals}`,
      `dropped=${stats.dropped}`,
      `fraction=${round(stats.fraction, 4)}`,
      `period=${round(stats.periodMs, 2)}`,
      `p50=${round(stats.p50Ms, 2)}`,
      `p95=${round(stats.p95Ms, 2)}`,
      `ms=${round(stats.recordedMs, 0)}`,
    ].join(";")
    : "n=0";
  return `\u001b]${FRAME_PROBE_OSC};frames;${body}\u0007`;
}
