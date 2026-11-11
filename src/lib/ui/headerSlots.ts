/**
 * Where a view's own toolbar may sit when the view bar has room for it.
 *
 * `ViewContextBar` is drawn by the views (History's list segment, Work's
 * conflict count), which the header does not own. At the widest layout the
 * header's second row has spare width, so the bar hands its node to a slot the
 * view bar registers here instead of costing the pane a 40px row of its own.
 * Below `FOLD_MIN_WIDTH` the slot is ignored and the bar keeps its row.
 *
 * The view bar measures itself and says whether it is wide enough: the
 * decision follows the row's own width (a container query's rule), not the
 * window's, so a window wider than its stage does not fold into a narrow row.
 */
import { derived, writable, type Readable } from "svelte/store";

/** The view bar width at which it has room for the context content. */
export const FOLD_MIN_WIDTH = 1600;

const slot = writable<HTMLElement | null>(null);
const wide = writable(false);

/** The element the view bar offers, or null when it is not on screen. */
export const contextSlot: Readable<HTMLElement | null> = { subscribe: slot.subscribe };

/**
 * Registers the view bar's slot for as long as the returned disposer is not
 * called. A second registration replaces the first; the first one's disposer
 * then leaves the second alone.
 */
export function registerContextSlot(node: HTMLElement): () => void {
  slot.set(node);
  return () => {
    slot.update((current) => (current === node ? null : current));
    wide.set(false);
  };
}

/** Called by the view bar with its measured inline size. */
export function reportViewBarWidth(width: number): void {
  wide.set(width >= FOLD_MIN_WIDTH);
}

/** The slot, only while the bar is wide enough to take the content: otherwise null. */
export const foldTarget: Readable<HTMLElement | null> = derived(
  [slot, wide],
  ([$slot, $wide]) => ($wide ? $slot : null),
);
