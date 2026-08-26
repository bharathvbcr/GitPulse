/**
 * Viewport-fit positioning for context menus.
 *
 * The old BranchList menu guessed its own size ("innerWidth - 200"), which
 * let long menus paint off the bottom edge and short ones overlap their
 * click anchor pointlessly. This helper takes the menu's MEASURED size and
 * clamps the anchor so the fully-rendered rect stays inside the viewport —
 * including never off-screen left/top, which naive Math.min flips forget.
 */

export interface MenuPosition {
  left: number;
  top: number;
}

/**
 * Coerce any coordinate/size to a finite non-negative number: NaN compares
 * false against every bound and would silently skip clamping; Infinity and
 * negatives have no meaningful geometry. Zero is the safe identity for all
 * of them — an unknown size just means "no fit correction possible".
 */
function safeGeometry(v: number): number {
  return Number.isFinite(v) && v > 0 ? v : 0;
}

/**
 * Clamp a desired anchor `(x, y)` so a `menuW × menuH` rect opened there
 * fits inside a `viewportW × viewportH` viewport. Overflow off the right/
 * bottom edge pulls the menu back by its own size; a negative result (menu
 * larger than the viewport) flushes to 0 instead of sitting off-screen.
 * Hostile inputs (NaN, ±Infinity, huge values, zero-sized viewport) are
 * sanitized first, so the output is always finite with `0 ≤ left ≤ viewportW`.
 */
export function clampMenuPosition(
  x: number,
  y: number,
  menuW: number,
  menuH: number,
  viewportW: number,
  viewportH: number
): MenuPosition {
  const px = safeGeometry(x);
  const py = safeGeometry(y);
  const w = safeGeometry(menuW);
  const h = safeGeometry(menuH);
  const vw = safeGeometry(viewportW);
  const vh = safeGeometry(viewportH);

  let left = px;
  if (left + w > vw) left = vw - w;
  if (left < 0) left = 0;

  let top = py;
  if (top + h > vh) top = vh - h;
  if (top < 0) top = 0;

  return { left, top };
}
