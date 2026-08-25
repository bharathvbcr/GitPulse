/**
 * Focus math for roving-tabindex lists (ARIA tabs, toolbars).
 *
 * `current` is the index of the focused item, or any out-of-range value
 * (including -1 when focus sits on the list container itself): entering or
 * re-entering the list honors travel direction — forward/Home land on the
 * first item, backward/End on the last. Within the list, ArrowLeft/ArrowRight
 * wrap around. Returns null only when there is nothing to move to.
 */
export type RovingKey = "ArrowLeft" | "ArrowRight" | "Home" | "End";

export function nextRovingIndex(current: number, count: number, key: RovingKey): number | null {
  if (count <= 0 || !Number.isInteger(count)) return null;
  if (!Number.isInteger(current) || current < 0 || current >= count) {
    return key === "ArrowLeft" || key === "End" ? count - 1 : 0;
  }
  switch (key) {
    case "ArrowRight":
      return (current + 1) % count;
    case "ArrowLeft":
      return (current - 1 + count) % count;
    case "Home":
      return 0;
    case "End":
      return count - 1;
  }
}
