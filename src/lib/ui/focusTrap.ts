export interface FocusableProbe {
  hasAttribute(qualifiedName: string): boolean;
  getAttribute(qualifiedName: string): string | null;
}

/**
 * Elements that can receive keyboard focus, in the order Tab would visit
 * them. `[tabindex="-1"]` is excluded (programmatically focusable only) and
 * `disabled` form controls are excluded at the selector level.
 */
export const FOCUSABLE_SELECTOR = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "iframe",
  "[tabindex]:not([tabindex='-1'])",
  "[contenteditable='true']",
  "[contenteditable='']",
].join(", ");

function isHiddenFromTabOrder(el: FocusableProbe): boolean {
  if (el.hasAttribute("hidden")) return true;
  if (el.getAttribute("aria-hidden") === "true") return true;
  return false;
}

/**
 * Enumerate the focusable descendants of `container` in DOM order, skipping
 * elements hidden from the accessibility tree. Layout-based hiding
 * (`display: none`) cannot be detected without a live renderer; cycling
 * compensates by re-verifying that each `.focus()` actually moved focus.
 *
 * Pure apart from reading the container: safe to exercise with fixture
 * objects in unit tests.
 */
export function enumerateFocusables<T extends FocusableProbe>(
  container: { querySelectorAll(selectors: string): ArrayLike<T> },
): T[] {
  const found: T[] = [];
  const candidates = container.querySelectorAll(FOCUSABLE_SELECTOR);
  for (let i = 0; i < candidates.length; i += 1) {
    const candidate = candidates[i];
    if (!isHiddenFromTabOrder(candidate)) found.push(candidate);
  }
  return found;
}

interface Documentish {
  activeElement: Element | null;
}

function ownerDocumentOf(node: Node): Documentish {
  return (
    (node.ownerDocument as unknown as Documentish | null) ??
    (typeof document === "undefined" ? { activeElement: null } : document)
  );
}

const wrap = (index: number, length: number): number =>
  ((index % length) + length) % length;

// instanceof against DOM globals must be guarded: this module is imported
// (and unit-tested) in plain Node where `HTMLElement`/`Element` are absent.
function isHtmlElement(value: unknown): value is HTMLElement {
  return typeof HTMLElement !== "undefined" && value instanceof HTMLElement;
}

/**
 * Move focus one step through `container`'s focusables, wrapping at both
 * ends. When focus currently sits outside the container it enters from the
 * front (forward) or back (backward). A candidate that silently fails to
 * take focus (e.g. visually hidden without [hidden]) is skipped, so the
 * cycle can never get stuck on an unfocusable match.
 */
export function cycleFocus(container: HTMLElement, forward: boolean): void {
  const items = enumerateFocusables(container);
  if (items.length === 0) {
    container.focus();
    return;
  }
  const doc = ownerDocumentOf(container);
  const current = doc.activeElement;
  // Membership by identity alone — no `instanceof Element`, which would
  // misfire in non-browser runtimes where the DOM globals are undefined.
  const currentIndex = items.findIndex((item) => item === current);
  // Stepping from -1 forward lands on 0; from length backward lands on the
  // last item — i.e. outside-focus enters the ring at its near edge.
  const start = currentIndex >= 0 ? currentIndex : forward ? -1 : items.length;

  for (let step = 1; step <= items.length; step += 1) {
    const index = wrap(forward ? start + step : start - step, items.length);
    const candidate = items[index];
    if (!isHtmlElement(candidate)) continue;
    candidate.focus();
    if (ownerDocumentOf(candidate).activeElement === candidate) return;
  }
  // Nothing accepted focus (fully hidden dialog?): park on the shell so
  // keyboard events keep flowing into the dialog subtree.
  container.focus();
}

export interface TrapFocusOptions {
  /**
   * Whether the action moves focus on mount. True (default) focuses
   * `initial()` when it returns an element, else the first focusable.
   * False hands initial-focus duty to the host component entirely.
   */
  autofocus?: boolean;
  /** Preferred initial-focus target; consulted only when autofocus is set. */
  initial?: () => HTMLElement | null;
}

/**
 * Svelte action: modal dialog focus management.
 *
 * On mount it records the previously focused element, moves focus inside
 * `node`, and intercepts Tab/Shift+Tab so keyboard focus cycles within the
 * dialog. On destroy the previously focused element regains focus — unless
 * it left the document meanwhile (its anchor dialog closed first).
 */
export function trapFocus(node: HTMLElement, options: TrapFocusOptions = {}) {
  if (typeof document === "undefined") {
    // No DOM (SSR import-time evaluation): nothing to attach or restore.
    return { update(_next: TrapFocusOptions) {}, destroy() {} };
  }

  let previous = isHtmlElement(document.activeElement)
    ? document.activeElement
    : null;
  let config = options;

  const placeInitialFocus = () => {
    if (!config.autofocus) return;
    const preferred = config.initial?.() ?? null;
    if (isHtmlElement(preferred)) {
      preferred.focus();
      return;
    }
    const first = enumerateFocusables(node).find(
      (el): el is HTMLElement => isHtmlElement(el),
    );
    if (first) first.focus();
    else if (node.hasAttribute("tabindex")) node.focus();
  };

  function onKeyDown(event: KeyboardEvent) {
    if (event.key !== "Tab" || event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    event.preventDefault();
    cycleFocus(node, !event.shiftKey);
  }

  node.addEventListener("keydown", onKeyDown);
  placeInitialFocus();

  return {
    update(next: TrapFocusOptions) {
      config = next;
    },
    destroy() {
      node.removeEventListener("keydown", onKeyDown);
      if (previous?.isConnected) previous.focus();
    },
  };
}
