import { setReduceMotionOverride } from "./easing";

/**
 * The single seam that applies the "reduce motion" preference.
 *
 * Motion is produced two ways in this app and both have to be told, or the
 * setting would half-work in a way that is hard to see: Svelte transitions
 * ask `prefersReducedMotion` at runtime, while view entrances, spinners and
 * the macOS control transitions are CSS animations gated on a media query.
 * So this writes the JS override AND stamps `data-motion` on `<html>`, which
 * app.css pairs with `(prefers-reduced-motion: reduce)` in every rule.
 *
 * Like the override itself the attribute is one-way — it is present only to
 * request *less* motion, never to force animation onto a reader whose system
 * asked for less.
 */
export const MOTION_ATTRIBUTE = "data-motion";
export const REDUCED = "reduced";

/** The element carrying the attribute; normally `<html>`. */
export interface MotionTarget {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
}

export function applyReduceMotion(reduce: boolean, target?: MotionTarget | null): void {
  setReduceMotionOverride(reduce);
  const root =
    target ?? (typeof document !== "undefined" ? document.documentElement : null);
  if (!root) return;
  if (reduce) root.setAttribute(MOTION_ATTRIBUTE, REDUCED);
  else root.removeAttribute(MOTION_ATTRIBUTE);
}
