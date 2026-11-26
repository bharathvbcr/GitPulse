/**
 * Which double-clicks in a docked Explorer open the full Explorer page.
 *
 * The Explorer docks in two places, the sidebar and the terminal, and both
 * hosts ask this one function so they cannot disagree about which targets
 * count.
 *
 * Only empty space and the pane's own chrome count: the tree below its last
 * row, the "Explorer" title strip, padding around the controls. Everything
 * the reader can already act on keeps its own double-click:
 *
 * - A file row's double-click keeps the file open where its click sent it
 *   (`resolveFileOpen`), so it pins in Code, a Files tab or a split. Making
 *   it also switch the page would pull a reader who chose "open in the
 *   terminal" away from the terminal. A folder row's two clicks fold it and
 *   unfold it.
 * - A field's double-click selects a word, and a rename is typed in a field.
 * - A row is draggable, and a drag starts on its own pointer events.
 * - The resize sash's double-click resets its width.
 * - Buttons, links, menus, tabs and search hits are controls in their own
 *   right.
 */

/**
 * Elements whose double-click belongs to themselves. Plain compound
 * selectors only: no `:not()`, so `closest()` cannot throw on a WebKit that
 * rejects a complex argument.
 */
export const EXPLORER_PAGE_GESTURE_EXCLUDED = [
  "button",
  "a[href]",
  "input",
  "textarea",
  "select",
  "label",
  "summary",
  '[contenteditable=""]',
  '[contenteditable="true"]',
  '[contenteditable="plaintext-only"]',
  '[draggable="true"]',
  '[role="treeitem"]',
  '[role="option"]',
  '[role="listbox"]',
  '[role="menu"]',
  '[role="menuitem"]',
  '[role="menuitemcheckbox"]',
  '[role="menuitemradio"]',
  '[role="separator"]',
  '[role="button"]',
  '[role="link"]',
  '[role="tab"]',
  '[role="switch"]',
  '[role="checkbox"]',
  '[role="radio"]',
  '[role="slider"]',
  '[role="textbox"]',
  '[role="searchbox"]',
  '[role="combobox"]',
  '[role="dialog"]',
].join(",");

interface ClosestCapable {
  closest(selector: string): unknown;
}

function canAskClosest(target: unknown): target is ClosestCapable {
  return (
    typeof target === "object" &&
    target !== null &&
    typeof (target as { closest?: unknown }).closest === "function"
  );
}

/**
 * True when this double-click landed on the docked Explorer's empty space or
 * chrome and should open the Explorer page. A handled event, a non-primary
 * button and a target that is not an element never do.
 */
export function opensExplorerPage(event: {
  target: EventTarget | null | unknown;
  defaultPrevented: boolean;
  button?: number;
}): boolean {
  if (event.defaultPrevented) return false;
  if (event.button !== undefined && event.button !== 0) return false;
  const { target } = event;
  if (!canAskClosest(target)) return false;
  return target.closest(EXPLORER_PAGE_GESTURE_EXCLUDED) === null;
}
