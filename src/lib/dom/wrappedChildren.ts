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

export interface WrappedChildrenParams {
  /** Anything that changes when the children do; a change re-counts. */
  key: unknown;
  onCount: (wrapped: number) => void;
}

/**
 * Svelte action for a one-line chip strip that wraps onto lines it clips.
 *
 * Reports `countWrapped` for the node's element children, and trims the
 * node to the extent of its first line while any have wrapped: a flex item
 * keeps the width the row gave it, so without the trim the chips that left
 * would leave their space behind as a gap.
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
  const measure = () => {
    node.style.maxWidth = "";
    const children = node.children as unknown as ArrayLike<HTMLElement>;
    const next = countWrapped(children);
    if (next > 0) {
      const lastOnLine = children[children.length - next - 1];
      const extent = lastOnLine
        ? lastOnLine.getBoundingClientRect().right - node.getBoundingClientRect().left
        : 0;
      // One pixel of slack so sub-pixel rounding never wraps the last chip
      // that fits; the next chip needs a whole gap more than that.
      node.style.maxWidth = `${Math.ceil(extent) + 1}px`;
    }
    if (next === last) return;
    last = next;
    current.onCount(next);
  };
  const observer = new ResizeObserver(measure);
  const observeAll = () => {
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
        observeAll();
        measure();
      });
    },
    destroy() {
      observer.disconnect();
    },
  };
}
