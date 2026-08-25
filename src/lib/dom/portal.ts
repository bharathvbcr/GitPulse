export type PortalTarget = string | HTMLElement;

/**
 * Resolve a portal destination without assuming a DOM exists. Under SSR or a
 * bare Node test environment (`typeof document === "undefined"`) there is
 * nowhere to move the node, so callers keep it in place instead of crashing.
 */
export function resolvePortalTarget(target: PortalTarget): HTMLElement | null {
  if (typeof document === "undefined") return null;
  const resolved =
    typeof target === "string" ? document.querySelector(target) : target;
  return resolved instanceof HTMLElement ? resolved : null;
}

/**
 * Svelte action: relocate a node to a high-level container (document.body by
 * default) on mount, and unmount it on destroy.
 *
 * Viewport-fixed popovers must live outside paint-contained subtrees such as
 * `.gp-pane`: `contain: paint` turns those elements into containing blocks for
 * `position: fixed` descendants (clipping the popover to the pane) and into
 * stacking contexts (trapping the popover below sibling panes regardless of
 * its own z-index). Rendering through the body sidesteps both.
 *
 * Destroy must *remove* the node, not put it back at the original parent.
 * Svelte 5's `destroy_effect()` walks `nodes.start..end` at the original
 * location *before* action teardown; a portaled node is no longer in that
 * range, so the walk misses it. Re-parenting to home would resurrect a
 * leftover overlay after the component has already closed it — a dropdown
 * that cannot dismiss, because closed state no longer owns the node.
 */
export function portal(node: HTMLElement, target: PortalTarget = "body") {
  const attach = (next: PortalTarget) => {
    const destination = resolvePortalTarget(next);
    if (destination && destination !== node.parentElement) {
      destination.appendChild(node);
    }
  };
  attach(target);
  return {
    update(next: PortalTarget) {
      target = next;
      attach(target);
    },
    destroy() {
      node.remove();
    },
  };
}
