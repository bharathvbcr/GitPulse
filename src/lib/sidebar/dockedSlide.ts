import { motionDuration, type MediaMatch } from "../motion/easing";
import { DURATION_MS, EASING, EXIT_EASE, EXIT_MS } from "../motion/tokens";

/**
 * The docked sidebar's open and close, on transform and opacity only.
 *
 * The column used to animate its own `width`, which re-laid-out the workspace on
 * every frame. Now the layout changes once, at the start of an expand and at the
 * end of a collapse, and the panel travels on the compositor:
 *
 * - `in`: the column is already wide; the panel arrives from the rail's edge.
 * - `out`: the column is still wide; the panel leaves toward the rail's edge, and
 *   the caller narrows the column when `onDone` runs.
 * - `settle`: the rail fades in once the column has narrowed.
 *
 * `travel` is how far the panel sits from its place at the start of `in` and at
 * the end of `out` (the open width less the rail's). Under reduced motion there
 * is no animation: `onDone` runs at once and `null` comes back. A cancelled
 * animation does not call `onDone`: the caller that cancels it has already
 * decided the final state.
 */
export type DockedSlide = "in" | "out" | "settle";

/** The properties a docked slide animates. The harness reads them back from `getAnimations()`. */
export const DOCKED_SLIDE_PROPERTIES: readonly string[] = Object.freeze(["transform", "opacity"]);

export function slideDocked(
  element: HTMLElement,
  slide: DockedSlide,
  travel: number,
  onDone: () => void,
  media?: MediaMatch | null,
): Animation | null {
  const entering = slide !== "out";
  const duration = motionDuration(slide === "in" ? DURATION_MS.fast : slide === "out" ? EXIT_MS.fast : DURATION_MS.instant, media);
  if (duration === 0 || typeof element.animate !== "function") {
    onDone();
    return null;
  }
  const away = `translate3d(${-Math.max(0, travel)}px, 0, 0)`;
  const home = "translate3d(0, 0, 0)";
  const frames: Keyframe[] =
    slide === "in"
      ? [{ transform: away, opacity: 0 }, { transform: home, opacity: 1 }]
      : slide === "out"
        ? [{ transform: home, opacity: 1 }, { transform: away, opacity: 0 }]
        : [{ opacity: 0 }, { opacity: 1 }];
  const animation = element.animate(frames, { duration, easing: entering ? EASING.effect : EXIT_EASE, fill: "both" });
  animation.onfinish = () => {
    onDone();
    // The caller's state change flushes in a microtask queued before this one,
    // so the held end frame is released only after the new DOM is in place.
    queueMicrotask(() => animation.cancel());
  };
  return animation;
}
