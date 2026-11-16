/**
 * Geometry for the terminal dock.
 *
 * Kept beside `sidebar/metrics.ts` and shaped the same way: the constants and
 * the clamp have one owner, so the drag handle, the keyboard step and the
 * persisted preference cannot drift apart. The arithmetic is `ui/paneHeight`,
 * shared with the commit details pane; this file supplies the dock's bounds.
 */

import { clampPaneHeight, fitPaneHeight, type PaneBounds } from "../ui/paneHeight";

export const TERMINAL_DOCK_MIN_HEIGHT = 200;
export const TERMINAL_DOCK_MAX_HEIGHT = 2400;
export const TERMINAL_DOCK_DEFAULT_HEIGHT = 280;
/** Keyboard resize step for the dock separator (ArrowUp/ArrowDown). */
export const TERMINAL_DOCK_RESIZE_STEP = 24;

export const TERMINAL_DOCK_BOUNDS: PaneBounds = {
  min: TERMINAL_DOCK_MIN_HEIGHT,
  max: TERMINAL_DOCK_MAX_HEIGHT,
  fallback: TERMINAL_DOCK_DEFAULT_HEIGHT,
};

/**
 * Clamp a requested dock height to the supported range. A non-finite value
 * falls back to the default; the rest is `clampPaneHeight`.
 */
export function clampTerminalDockHeight(px: number): number {
  return clampPaneHeight(px, TERMINAL_DOCK_BOUNDS);
}

/**
 * The largest dock that still leaves the view above it usable.
 *
 * A dock dragged to the top of the window is a terminal that has silently
 * replaced the app; the view keeps at least `minViewHeight` so the dock can
 * never become the whole screen by accident.
 */
export function fitTerminalDockHeight(
  requested: number,
  containerHeight: number,
  minViewHeight = 160,
): number {
  return fitPaneHeight(requested, containerHeight, minViewHeight, TERMINAL_DOCK_BOUNDS);
}
