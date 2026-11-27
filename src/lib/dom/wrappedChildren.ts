/**
 * How many of a wrapping flex row's children have flowed past its first line.
 *
 * A one-line strip of chips that must never show a chip cut in half lets the
 * strip wrap and clips every line after the first: each chip is then either
 * whole or entirely out of view. What that cannot say on its own is how many
 * went out of view, which is this count. Items flow in order, so the ones off
 * the first line are always the last `n`.
 */

/** Children whose top sits below the first child's line. */
export function countWrapped(children: ArrayLike<{ offsetTop: number }>): number {
  if (children.length === 0) return 0;
  const firstLine = children[0].offsetTop;
  let wrapped = 0;
  for (let i = 0; i < children.length; i++) {
    // Sub-pixel slack: chips on one line can differ in offsetTop by a rounding pixel.
    if (children[i].offsetTop > firstLine + 1) wrapped++;
  }
  return wrapped;
}

/**
 * How many of the strip's children are out of view: the ones that wrapped, or
 * every one of them when even the first does not fit. Flex wrapping never
 * moves a line's first item, so a strip narrower than its first chip keeps
 * that chip on the visible line and clips it; the chip is then not whole, and
 * whole-or-gone makes it gone, which takes every chip after it too.
 */
export function countHidden(
  children: ArrayLike<{ offsetTop: number }>,
  firstOverflows: boolean,
): number {
  if (children.length === 0) return 0;
  return firstOverflows ? children.length : countWrapped(children);
}

export interface WrappedChildrenParams {
  /** Anything that changes when the children do; a change re-counts. */
  key: unknown;
  onCount: (wrapped: number) => void;
}

/**
 * Svelte action for a one-line chip strip that wraps onto lines it clips.
 *
 * Reports `countHidden` for the node's element children, and trims the
 * node to the extent of its first line while any are hidden: a flex item
 * keeps the width the row gave it, so without the trim the chips that left
 * would leave their space behind as a gap. When the first chip itself does
 * not fit, the node collapses to nothing rather than show it cut. The
 * children must be free to overflow the node (no `max-width: 100%`), or a
 * chip squeezed to the node's width would never be seen not to fit.
 *
 * Re-measures on any resize of the node, of a child (a chip that widens when
 * its font arrives wraps without the strip resizing), or of the node's parent
 * (a wider row has room again, which the trimmed strip itself never sees),
 * and whenever `key` changes. Each measurement lifts the trim first, so it
 * always counts against the width the row would give, and it reports only
 * changes: a strip that never overflows never writes state.
 */
export function wrappedChildren(node: HTMLElement, params: WrappedChildrenParams) {
  let current = params;
  let last = -1;
  let destroyed = false;
  const measure = () => {
    if (destroyed) return;
    node.style.maxWidth = "";
    const children = node.children as unknown as ArrayLike<HTMLElement>;
    const box = node.getBoundingClientRect();
    const firstOverflows =
      children.length > 0 && children[0].getBoundingClientRect().right > box.right + 1;
    const next = countHidden(children, firstOverflows);
    if (next >= children.length && next > 0) {
      node.style.maxWidth = "0px";
    } else if (next > 0) {
      const lastOnLine = children[children.length - next - 1];
      const extent = lastOnLine.getBoundingClientRect().right - box.left;
      // One pixel of slack so sub-pixel rounding never wraps the last chip
      // that fits; the next chip needs a whole gap more than that.
      node.style.maxWidth = `${Math.ceil(extent) + 1}px`;
    }
    if (next === last) return;
    last = next;
    current.onCount(next);
  };
  // Without ResizeObserver (a DOM-less test environment) the strip is
  // measured once per key instead of on every resize.
  const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
  const observeAll = () => {
    if (!observer) return;
    observer.disconnect();
    observer.observe(node);
    if (node.parentElement) observer.observe(node.parentElement);
    for (const child of node.children) observer.observe(child);
  };
  observeAll();
  measure();
  return {
    update(next: WrappedChildrenParams) {
      const changed = next.key !== current.key;
      current = next;
      if (!changed) return;
      // The new children are in the DOM by the next microtask.
      queueMicrotask(() => {
        // The node may have left between the update and this microtask.
        if (destroyed) return;
        observeAll();
        measure();
      });
    },
    destroy() {
      destroyed = true;
      observer?.disconnect();
    },
  };
}
