export function clamp01(n: number): number {
  if (n <= 0) return 0;
  if (n >= 1) return 1;
  return n;
}

export function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

export function easeOutCubic(t: number): number {
  const x = clamp01(t);
  return 1 - (1 - x) ** 3;
}

/**
 * Exponential damping toward `target`. After `halfLifeMs`, the remaining
 * distance is halved — independent of frame rate, unlike a fixed lerp.
 */
export function damp(current: number, target: number, deltaMs: number, halfLifeMs = 70): number {
  if (halfLifeMs <= 0) return target;
  if (deltaMs <= 0) return current;
  if (deltaMs >= 1000) return target;
  const t = 1 - 2 ** (-deltaMs / halfLifeMs);
  return lerp(current, target, t);
}

export type MediaMatch = Pick<Window, "matchMedia">;

/**
 * The app's own "reduce motion" preference, layered over the system one.
 *
 * Deliberately one-way: it can only ever ADD reduction. A user who has asked
 * the operating system for less motion has asked every application, and a
 * checkbox in here must not be able to overrule that — so this is a boolean
 * that turns reduction on, never a tri-state that can turn it off.
 *
 * Module-level rather than threaded through every call site because the
 * transition helpers are called from Svelte templates during a transition,
 * where there is nothing to pass a store through.
 */
let reduceMotionOverride = false;

export function setReduceMotionOverride(reduce: boolean): void {
  reduceMotionOverride = reduce;
}

export function reduceMotionOverrideEnabled(): boolean {
  return reduceMotionOverride;
}

export function prefersReducedMotion(media: MediaMatch | null = defaultMedia()): boolean {
  if (reduceMotionOverride) return true;
  if (!media || typeof media.matchMedia !== "function") return false;
  return media.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function motionDuration(ms: number, media?: MediaMatch | null): number {
  return prefersReducedMotion(media) ? 0 : ms;
}

export function fadeParams(media?: MediaMatch | null): { duration: number } {
  return { duration: motionDuration(140, media) };
}

export function scaleParams(media?: MediaMatch | null): { duration: number; start: number } {
  return { duration: motionDuration(180, media), start: 0.97 };
}

function defaultMedia(): MediaMatch | null {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" ? window : null;
}
