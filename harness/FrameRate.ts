/**
 * Frame-rate measurement for the overhaul's motion decisions.
 *
 * Four phases, each timed with the `requestAnimationFrame` timestamp (the
 * frame's own start time, not `performance.now()` inside the callback, so
 * callback jitter does not read as a long frame):
 *
 *  - `idle`      nothing animates; the rAF cadence the engine offers at all.
 *  - `transform` a 280px panel slides and fades (translateX + opacity, WAAPI)
 *                over a dense 400-row list. Compositor-only properties.
 *  - `blur`      the same animation, with a 480x320 float surface wearing
 *                `shadow-float` over the list. Under `html.macos` the real
 *                float blur rule applies (blur(34px) saturate brightness),
 *                and the float overlaps the sliding panel so the pixels it
 *                samples change every frame: the worst case for a backdrop
 *                filter, not a cached one.
 *  - `width`     a docked panel beside the list animates `width` 0 -> 280px.
 *                The list is a flex sibling, so every frame is a layout.
 *
 * This is a measurement page, not a product surface. It deliberately ignores
 * `prefers-reduced-motion` and the app's `data-motion` preference: it animates
 * through the Web Animations API, which neither switch touches, and a
 * measurement that skipped the motion it is measuring would report nothing.
 */

export type PhaseName = "idle" | "transform" | "blur" | "width";
export const PHASES: readonly PhaseName[] = ["idle", "transform", "blur", "width"];

/** Why a phase's numbers cannot be trusted; "ok" is the only usable state. */
export type PhaseStatus = "ok" | "hidden" | "stalled" | "too-few-frames" | "animation-did-not-run" | "float-blur-not-applied";

export type FocusState = "focused" | "unfocused" | "mixed";

export interface FrameStats {
  frames: number;
  medianMs: number;
  p95Ms: number;
  p99Ms: number;
  maxMs: number;
  /** 1000 / median interval. */
  hz: number;
  /** 1000 / mean interval. WebKit rounds timestamps to whole milliseconds, so a
   * 16.7 ms cadence reads as a 17 ms median (58.8 Hz); the mean over the whole
   * phase is not quantised the same way. */
  meanHz: number;
  /** Intervals longer than 1.5x the median. */
  longFrames: number;
}

export interface PhaseResult extends FrameStats {
  phase: PhaseName;
  status: PhaseStatus;
  focus: FocusState;
  /** Why the status is not "ok", for the verdict and the page. */
  note: string;
}

/** Nearest-rank percentile of an ascending list. */
function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return Number.NaN;
  const rank = Math.min(sorted.length, Math.max(1, Math.ceil((p / 100) * sorted.length)));
  return sorted[rank - 1];
}

/** Summarise frame intervals (ms). An empty list yields NaN statistics, which
 * the checks refuse, so "no data" can never read as a good number. */
export function summarize(intervals: number[]): FrameStats {
  const sorted = [...intervals].sort((a, b) => a - b);
  const medianMs = percentile(sorted, 50);
  return {
    frames: sorted.length,
    medianMs,
    p95Ms: percentile(sorted, 95),
    p99Ms: percentile(sorted, 99),
    maxMs: sorted.length ? sorted[sorted.length - 1] : Number.NaN,
    hz: 1000 / medianMs,
    meanHz: sorted.length ? 1000 / (sorted.reduce((sum, v) => sum + v, 0) / sorted.length) : Number.NaN,
    longFrames: sorted.filter(v => v > medianMs * 1.5).length,
  };
}

interface Sample {
  stamps: number[];
  hidden: boolean;
  focusSeen: Set<boolean>;
  stalled: boolean;
}

/** Collect rAF timestamps for `durationMs` of frame time. A wall-clock guard
 * ends a run whose frames stop arriving, so a page that is never painted
 * reports `stalled` instead of hanging until the runner's deadline. */
function sampleFrames(durationMs: number): Promise<Sample> {
  return new Promise(resolve => {
    const sample: Sample = { stamps: [], hidden: document.hidden, focusSeen: new Set([document.hasFocus()]), stalled: false };
    let finished = false;
    const finish = (stalled: boolean) => {
      if (finished) return;
      finished = true;
      clearTimeout(guard);
      document.removeEventListener("visibilitychange", onVisibility);
      sample.stalled = stalled;
      sample.focusSeen.add(document.hasFocus());
      resolve(sample);
    };
    const onVisibility = () => { if (document.hidden) sample.hidden = true; };
    document.addEventListener("visibilitychange", onVisibility);
    const guard = setTimeout(() => finish(true), durationMs + 3000);
    const step = (stamp: number) => {
      if (finished) return;
      if (document.hidden) sample.hidden = true;
      sample.stamps.push(stamp);
      if (stamp - sample.stamps[0] >= durationMs) finish(false);
      else requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  });
}

/** Frames spent starting the animation and laying out the first frames; their
 * intervals describe set-up, not steady state. */
const WARMUP_MS = 250;
const MIN_FRAMES = 30;

function focusState(seen: Set<boolean>): FocusState {
  if (seen.size > 1) return "mixed";
  return seen.has(true) ? "focused" : "unfocused";
}

const nanStats = (): FrameStats => summarize([]);

async function measure(phase: PhaseName, durationMs: number, animation: Animation | null, preflight: string | null): Promise<PhaseResult> {
  if (preflight) return { phase, ...nanStats(), status: "float-blur-not-applied", focus: focusState(new Set([document.hasFocus()])), note: preflight };
  const sample = await sampleFrames(durationMs);
  const focus = focusState(sample.focusSeen);
  const base = { phase, focus };
  if (sample.hidden) return { ...base, ...nanStats(), status: "hidden", note: "the document was hidden during the phase; the engine withholds animation frames from a hidden page" };
  if (sample.stalled || sample.stamps.length === 0) {
    return { ...base, ...nanStats(), status: "stalled", note: `requestAnimationFrame delivered ${sample.stamps.length} frame(s) in ${durationMs + 3000} ms` };
  }
  const intervals: number[] = [];
  for (let i = 1; i < sample.stamps.length; i += 1) {
    if (sample.stamps[i] - sample.stamps[0] >= WARMUP_MS) intervals.push(sample.stamps[i] - sample.stamps[i - 1]);
  }
  const stats = summarize(intervals);
  if (intervals.length < MIN_FRAMES) return { ...base, ...stats, status: "too-few-frames", note: `${intervals.length} intervals after warm-up; ${MIN_FRAMES} needed` };
  if (animation) {
    const time = Number(animation.currentTime ?? 0);
    if (animation.playState !== "running" || time < durationMs * 0.8) {
      return { ...base, ...stats, status: "animation-did-not-run", note: `animation was ${animation.playState} at ${time.toFixed(0)} ms of ${durationMs} ms` };
    }
  }
  return { ...base, ...stats, status: "ok", note: "" };
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, css: string, className = "", text = ""): HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  node.style.cssText = css;
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
};

const frames = (n: number) => new Promise<void>(resolve => {
  const step = (left: number) => (left === 0 ? resolve() : requestAnimationFrame(() => step(left - 1)));
  step(n);
});

/** A commit-list-like row: dot, short id, a truncated message, author, age. */
function row(i: number): HTMLElement {
  const node = el("div", "display:flex;align-items:center;gap:10px;height:28px;padding:0 12px;border-bottom:1px solid rgb(var(--c-border) / 0.5);font-size:12px;white-space:nowrap");
  node.append(
    el("span", "flex:none;width:8px;height:8px;border-radius:9999px;background:rgb(var(--c-accent))"),
    el("span", "flex:none;width:56px;font-family:ui-monospace,monospace;opacity:.7", "", (0x1a2b3c4d + i * 7919).toString(16).slice(0, 7)),
    el("span", "flex:1 1 0;min-width:0;overflow:hidden;text-overflow:ellipsis", "", `row ${i}: adjust the tab strip, keep the focus ring, and reflow the sidebar when the window narrows`),
    el("span", "flex:none;width:96px;opacity:.7", "", "Dana Whitfield"),
    el("span", "flex:none;width:48px;text-align:right;opacity:.7", "", `${(i % 59) + 1}m`),
  );
  return node;
}

function panelBody(): HTMLElement {
  const body = el("div", "height:100%;padding:12px;box-sizing:border-box;overflow:hidden;font-size:12px;border-right:1px solid rgb(var(--c-border))", "bg-surface");
  for (let i = 0; i < 14; i += 1) body.append(el("div", "padding:6px 0;white-space:nowrap", "", `Panel item ${i + 1}`));
  return body;
}

export interface FrameRateEnvironment {
  userAgent: string;
  devicePixelRatio: number;
  screen: string;
  reducedMotionPreference: boolean;
  macosClass: boolean;
  floatBlur: string;
}

export interface FrameRateRun {
  phases: PhaseResult[];
  environment: FrameRateEnvironment;
}

/** Run the four phases against `stage`, which this function owns for the run. */
export async function runFrameRate(stage: HTMLElement, durationMs = 3000): Promise<FrameRateRun> {
  const root = document.documentElement;
  const results: PhaseResult[] = [];
  let floatBlur = "";

  const build = (docked: boolean) => {
    stage.replaceChildren();
    const dock = el("div", "flex:none;width:0;height:100%;overflow:hidden");
    const list = el("div", "flex:1 1 0;min-width:0;height:100%;overflow:hidden", "bg-background");
    for (let i = 0; i < 400; i += 1) list.append(row(i));
    if (docked) { dock.append(panelBody()); stage.append(dock, list); } else stage.append(list);
    return { dock, list };
  };
  const overlayPanel = () => {
    const panel = el("div", "position:absolute;left:0;top:0;bottom:0;width:280px;will-change:transform,opacity");
    panel.append(panelBody());
    stage.append(panel);
    return panel;
  };
  const slide = (panel: HTMLElement) => panel.animate(
    { transform: ["translateX(-280px)", "translateX(0)"], opacity: [0.2, 1] },
    { duration: 700, iterations: Infinity, direction: "alternate", easing: "ease-in-out" },
  );
  const rest = async () => { await frames(4); await new Promise(resolve => setTimeout(resolve, 250)); await frames(2); };

  // idle
  build(false);
  await rest();
  results.push(await measure("idle", durationMs, null, null));

  // transform
  build(false);
  let animation = slide(overlayPanel());
  await rest();
  results.push(await measure("transform", durationMs, animation, null));
  animation.cancel();

  // blur
  build(false);
  const panel = overlayPanel();
  const float = el("div", "position:absolute;left:60px;top:120px;width:480px;height:320px;padding:16px;box-sizing:border-box;font-size:13px", "shadow-float", "Float surface: menu, popover or dialog card over live content.");
  stage.append(float);
  const computed = getComputedStyle(float);
  floatBlur = computed.backdropFilter || computed.getPropertyValue("-webkit-backdrop-filter") || "none";
  const unblurred = !/blur\(/.test(floatBlur);
  animation = slide(panel);
  await rest();
  results.push(await measure("blur", durationMs, animation, unblurred ? `the float surface computed backdrop-filter "${floatBlur}"; html.macos and the float blur rule did not apply, so this phase would measure no blur` : null));
  animation.cancel();

  // width
  const { dock } = build(true);
  animation = dock.animate({ width: ["0px", "280px"] }, { duration: 700, iterations: Infinity, direction: "alternate", easing: "ease-in-out" });
  await rest();
  results.push(await measure("width", durationMs, animation, null));
  animation.cancel();

  stage.replaceChildren();
  return {
    phases: results,
    environment: {
      userAgent: navigator.userAgent,
      devicePixelRatio: window.devicePixelRatio,
      screen: `${screen.width}x${screen.height}`,
      reducedMotionPreference: matchMedia("(prefers-reduced-motion: reduce)").matches,
      macosClass: root.classList.contains("macos"),
      floatBlur,
    },
  };
}
