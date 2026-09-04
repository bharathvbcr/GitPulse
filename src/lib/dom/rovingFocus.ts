/**
 * Focus math for roving-tabindex lists (ARIA tabs, toolbars).
 *
 * `current` is the index of the focused item, or any out-of-range value
 * (including -1 when focus sits on the list container itself): entering or
 * re-entering the list honors travel direction — forward/Home land on the
 * first item, backward/End on the last. Within the list, the arrow keys wrap
 * around. Returns null only when there is nothing to move to.
 *
 * Both orientations are handled by one function because the math is the same
 * one: a vertical list's ArrowUp/ArrowDown mean exactly what a horizontal
 * one's ArrowLeft/ArrowRight do. A caller passes whichever key it received and
 * gets null for the keys its own orientation does not own, so a vertical list
 * does not swallow ArrowLeft and a horizontal one does not swallow ArrowUp.
 */
export type RovingKey =
  | "ArrowLeft"
  | "ArrowRight"
  | "ArrowUp"
  | "ArrowDown"
  | "Home"
  | "End";

export type RovingOrientation = "horizontal" | "vertical";

/** The arrow key an orientation moves backward with; forward is the other. */
function arrowDirection(
  key: RovingKey,
  orientation: RovingOrientation,
): "back" | "forward" | null {
  if (orientation === "horizontal") {
    if (key === "ArrowLeft") return "back";
    if (key === "ArrowRight") return "forward";
    return null;
  }
  if (key === "ArrowUp") return "back";
  if (key === "ArrowDown") return "forward";
  return null;
}

export function nextRovingIndex(
  current: number,
  count: number,
  key: RovingKey,
  orientation: RovingOrientation = "horizontal",
): number | null {
  if (count <= 0 || !Number.isInteger(count)) return null;
  // Home and End name an end of the list outright, so they answer the same
  // way whether or not focus is already inside it.
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  const direction = arrowDirection(key, orientation);
  if (direction === null) return null;
  if (!Number.isInteger(current) || current < 0 || current >= count) {
    // Entering the list: travel direction is preserved across the boundary.
    return direction === "back" ? count - 1 : 0;
  }
  return direction === "forward" ? (current + 1) % count : (current - 1 + count) % count;
}
