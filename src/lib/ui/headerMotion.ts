import type { TransitionConfig } from "svelte/transition";
import { motionDuration, type MediaMatch } from "../motion/easing";
import { DURATION_MS, EASING, EXIT_EASE, EXIT_MS } from "../motion/tokens";
import { cubicBezierEasing, springEasing } from "./transitions";

/**
 * The header's motion (HEADER_SPEC section 6), in one place.
 *
 * Every piece is transform and opacity only: nothing here animates width,
 * height, top or left, so a tab opening never re-lays-out the strip while it
 * plays. Durations and curves are the motion tokens (`DURATION_MS`, `EXIT_MS`,
 * the spatial spring and the exit curve), none a literal of this file. An
 * entrance plays at `fast` (140 ms) over the spatial spring; an exit is
 * `EXIT_MS.fast` (70 ms, the cap every exit shares) over the exit curve. Reduced
 * motion (the system preference or the in-app setting) zeroes both through
 * `motionDuration`, so a state change is then instant and nothing is skipped.
 *
 * What moves:
 * - a repository tab opening and closing: `tabEnter` / `tabExit`;
 * - the overflow menu and the hover card: `popupEnter` / `popupExit`;
 * - the attention mark and the Fetch-all pill with its progress ring:
 *   `markEnter` / `markExit`;
 * - row 2 swapping under a global surface: `swapRow` (a Web Animation on the
 *   row's controls, because they stay mounted and only change which they are).
 */

export const HEADER_ENTER_MS = DURATION_MS.fast;
export const HEADER_EXIT_MS = EXIT_MS.fast;

const spatial = springEasing("spatial");
const exit = cubicBezierEasing(...(EXIT_EASE.match(/-?[\d.]+/g)!.map(Number) as [number, number, number, number]));

/** Transform and opacity at progress `t` (0 hidden, 1 shown). */
type Frame = (t: number) => string;

function transition(frame: Frame, entering: boolean, media?: MediaMatch | null): TransitionConfig {
  return {
    duration: motionDuration(entering ? HEADER_ENTER_MS : HEADER_EXIT_MS, media),
    easing: entering ? spatial : exit,
    css: frame,
  };
}

const tabFrame: Frame = (t) => `opacity: ${Math.min(1, Math.max(0, t))}; transform: scale(${0.94 + 0.06 * t});`;
const popupFrame: Frame = (t) => `opacity: ${Math.min(1, Math.max(0, t))}; transform: translate3d(0, ${(1 - t) * -4}px, 0) scale(${0.985 + 0.015 * t});`;
const markFrame: Frame = (t) => `opacity: ${Math.min(1, Math.max(0, t))}; transform: scale(${0.6 + 0.4 * t});`;

/** The frames by name, so a test can play any of them under a given media preference. */
export const HEADER_FRAMES = { tab: tabFrame, popup: popupFrame, mark: markFrame } as const;

export function headerTransition(kind: keyof typeof HEADER_FRAMES, entering: boolean, media?: MediaMatch | null): TransitionConfig {
  return transition(HEADER_FRAMES[kind], entering, media);
}

// Svelte calls a transition as (node, params, { direction }), so these take no
// media argument: they read the live preference, as every other transition does.
export const tabEnter = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("tab", true);
export const tabExit = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("tab", false);
export const popupEnter = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("popup", true);
export const popupExit = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("popup", false);
export const markEnter = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("mark", true);
export const markExit = (_node?: Element, _params?: unknown, _options?: unknown) => headerTransition("mark", false);

/**
 * Fades row 2's controls out or in as the global surface changes. The row keeps
 * its place (the workspace below never jumps), so only opacity moves. Returns
 * the animation, or null when reduced motion asks for none; `onDone` runs when
 * the state is final either way, so the caller applies `visibility` after the
 * fade and never leaves a half-faded row.
 */
export function swapRow(
  element: HTMLElement,
  show: boolean,
  onDone: () => void,
  media?: MediaMatch | null,
): Animation | null {
  const duration = motionDuration(show ? HEADER_ENTER_MS : HEADER_EXIT_MS, media);
  if (duration === 0 || typeof element.animate !== "function") {
    onDone();
    return null;
  }
  const animation = element.animate(
    { opacity: show ? [0, 1] : [1, 0] },
    { duration, easing: show ? EASING.effect : EXIT_EASE, fill: "both" },
  );
  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    onDone();
  };
  animation.onfinish = () => {
    finish();
    animation.cancel();
  };
  animation.oncancel = finish;
  return animation;
}
