/**
 * Geometry for the terminal dock.
 *
 * Kept beside `sidebar/metrics.ts` and shaped the same way: the constants and
 * the clamp have one owner, so the drag handle, the keyboard step and the
 * persisted preference cannot drift apart.
 */

export const TERMINAL_DOCK_MIN_HEIGHT = 120;
export const TERMINAL_DOCK_MAX_HEIGHT = 900;
export const TERMINAL_DOCK_DEFAULT_HEIGHT = 280;
/** Keyboard resize step for the dock separator (ArrowUp/ArrowDown). */
export const TERMINAL_DOCK_RESIZE_STEP = 24;

/**
 * Clamp a requested dock height to the supported range.
 *
 * Fail-closed on hostile input: a non-finite value falls back to the default
 * rather than poisoning a persisted preference, which is exactly how a stored
 * `NaN` height would render the dock as a zero-pixel strip the user cannot
 * grab to fix.
 */
export function clampTerminalDockHeight(px: number): number {
  if (!Number.isFinite(px)) return TERMINAL_DOCK_DEFAULT_HEIGHT;
  if (px < TERMINAL_DOCK_MIN_HEIGHT) return TERMINAL_DOCK_MIN_HEIGHT;
  if (px > TERMINAL_DOCK_MAX_HEIGHT) return TERMINAL_DOCK_MAX_HEIGHT;
  // Whole pixels: fractional heights from sub-pixel drag deltas make the
  // flex layout shimmer between paints, and xterm re-fits on every change.
  return Math.round(px);
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
  const clamped = clampTerminalDockHeight(requested);
  if (!Number.isFinite(containerHeight) || containerHeight <= 0) return clamped;
  const ceiling = containerHeight - minViewHeight;
  if (ceiling < TERMINAL_DOCK_MIN_HEIGHT) return TERMINAL_DOCK_MIN_HEIGHT;
  return Math.min(clamped, Math.round(ceiling));
}
