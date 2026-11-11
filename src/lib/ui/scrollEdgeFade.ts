import {
  observeOverflow,
  overflowFadeToken,
  setOverflowFade,
  type OverflowAxis,
} from "../dom/overflowHint";

/**
 * Softens the edge of a dialog body that still hides content.
 *
 * A dialog body is a scroller with a hard clip, so a control that straddles
 * the clip line (a slider thumb, the last row of a list) is sliced in half and
 * reads as a rendering fault. This writes the same `data-gp-fade` token
 * `ScrollCue` writes, so app.css masks the body's own content to transparent
 * over the last two rem of each edge that still hides something, and clears it
 * at the end of the scroll, where nothing is hidden and the last row has to be
 * whole. Pair it with `scroll-py-8` on the body: the fade band is 2rem, and
 * without scroll padding a focused control scrolls in flush with the edge,
 * underneath the band.
 *
 * No chevron: a dialog's scrollbar already says it scrolls. `ScrollCue` is for
 * strips that have none.
 */
export function scrollEdgeFade(node: HTMLElement, axis: OverflowAxis = "y") {
  const stop = observeOverflow(node, axis, (hint) => {
    setOverflowFade(node, overflowFadeToken(axis, hint));
  });
  return {
    destroy() {
      stop();
      setOverflowFade(node, null);
    },
  };
}
