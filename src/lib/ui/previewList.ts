/**
 * Shared collapse/expand for long rail listings.
 *
 * Workflows, run cards, and the delivery timeline all preview then expand.
 * One implementation is how "show all" stays one meaning; a copy per section
 * is how two lists end up disagreeing.
 *
 * This slices what is already on screen. It is not a filter and must never be
 * fed to anything that decides scope — a live poll, a pass rate, a median, or
 * a bar scale.
 */

/**
 * How many rows a collapsed list will actually draw.
 *
 * Zero, negative, and non-finite counts would otherwise collapse a real
 * listing to nothing, which reads as "this repository has none of these".
 * Flooring at one keeps a collapsed section from lying; `overflowsPreview`
 * uses the same floor so the expander cannot appear for a list it would
 * not hide anything from.
 */
export function previewCap(previewCount: number): number {
  if (!Number.isFinite(previewCount)) return 1;
  return Math.max(1, Math.trunc(previewCount));
}

/**
 * The rows to render for `expanded`.
 *
 * Never empty for a non-empty list, whatever the preview count: a default
 * collapse that renders zero rows reads as "this repository has none of
 * these", which is the one thing a collapsed section must never say.
 */
export function previewSlice<T>(
  all: readonly T[],
  expanded: boolean,
  previewCount: number,
): T[] {
  if (expanded) return [...all];
  return all.slice(0, previewCap(previewCount));
}

/** Whether the list is long enough to be worth collapsing at all. */
export function overflowsPreview(total: number, previewCount: number): boolean {
  if (!Number.isFinite(total) || total <= 0) return false;
  return total > previewCap(previewCount);
}

/**
 * Label for the control that expands or collapses a previewed list.
 *
 * The expand label carries the count that was *fetched*, which is not
 * necessarily everything the repository has: when the backend capped the
 * listing, the section's own truncation notice says so separately. This
 * control never speaks for the rows the backend withheld.
 */
export function expandLabel(total: number, expanded: boolean, noun: string): string {
  if (expanded) return "Show fewer";
  const count = Number.isFinite(total) ? Math.max(0, Math.trunc(total)) : 0;
  const word = typeof noun === "string" && noun.trim() !== "" ? noun : "items";
  return `Show all ${count.toLocaleString()} ${word}`;
}
