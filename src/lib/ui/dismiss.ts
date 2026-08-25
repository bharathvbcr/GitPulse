/**
 * Overlay dismiss helpers. Duck-typed so Node tests can exercise the same
 * `closest` walk real pointer events use, without a DOM environment.
 */

export interface ClosestHost {
  closest(selector: string): unknown;
}

function hasClosest(value: unknown): value is ClosestHost {
  return (
    typeof value === "object" &&
    value !== null &&
    "closest" in value &&
    typeof (value as ClosestHost).closest === "function"
  );
}

/**
 * Element associated with an event target. Text nodes (and other non-elements)
 * walk to `parentElement` so `closest` is always safe to call.
 */
export function eventTargetElement(target: unknown): ClosestHost | null {
  if (hasClosest(target)) return target;
  if (typeof target === "object" && target !== null && "parentElement" in target) {
    const parent = (target as { parentElement: unknown }).parentElement;
    if (hasClosest(parent)) return parent;
  }
  return null;
}

/**
 * True when the pointer landed outside `insideSelector`. A missing target is
 * treated as outside (fail closed: dismiss).
 */
export function shouldDismissOverlay(target: unknown, insideSelector: string): boolean {
  const el = eventTargetElement(target);
  if (!el) return true;
  return el.closest(insideSelector) == null;
}
