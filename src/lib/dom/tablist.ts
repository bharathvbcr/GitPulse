import { nextRovingIndex, type RovingKey } from "./rovingFocus";

/**
 * The keyboard half of `role="tablist"`.
 *
 * Three tablists in this app declared the role and implemented none of what it
 * promises: no `aria-controls`, no `tabpanel` anywhere, every tab in the tab
 * order, and arrow keys doing nothing. A screen reader announced "tab, 2 of 4"
 * and then the behaviour that phrase implies was absent — which is worse than
 * plain buttons, because the announcement is a promise.
 *
 * Both halves live here so the four call sites cannot drift: `tabProps` for the
 * per-tab attributes and `handleTablistKeydown` for the movement.
 */

/** Keys this helper acts on; everything else falls through to the caller. */
const HANDLED: readonly string[] = ["ArrowLeft", "ArrowRight", "Home", "End"];

export function isTablistKey(key: string): key is RovingKey {
  return HANDLED.includes(key);
}

export interface TabAttributes {
  role: "tab";
  "aria-selected": boolean;
  "aria-controls": string;
  id: string;
  /**
   * Only the selected tab is reachable with Tab; the rest are reached with the
   * arrow keys. Leaving every tab tabbable is what makes a ten-tab strip cost
   * ten presses to step past.
   */
  tabindex: 0 | -1;
}

/** Attributes for one tab in a tablist named `group`. */
export function tabProps(group: string, id: string, selected: boolean): TabAttributes {
  return {
    role: "tab",
    "aria-selected": selected,
    "aria-controls": panelId(group, id),
    id: tabId(group, id),
    tabindex: selected ? 0 : -1,
  };
}

export function tabId(group: string, id: string): string {
  return `${group}-tab-${id}`;
}

export function panelId(group: string, id: string): string {
  return `${group}-panel-${id}`;
}

export interface TablistMove {
  /** Index to select and focus. */
  index: number;
}

/**
 * Resolves an arrow/Home/End press within a tablist.
 *
 * Returns null when the key is not one this pattern owns, so the caller's own
 * handler still sees it. Selection follows focus, which is the ARIA default
 * for tablists whose panels are cheap to show — every panel here is already
 * mounted or lazily cached, so there is no reason to make the user press Enter.
 */
export function handleTablistKeydown(
  key: string,
  currentIndex: number,
  count: number,
): TablistMove | null {
  if (!isTablistKey(key)) return null;
  const index = nextRovingIndex(currentIndex, count, key, "horizontal");
  return index === null ? null : { index };
}

/**
 * Moves DOM focus onto the tab at `index` within `container`.
 *
 * Focus is moved explicitly because roving tabindex changes which element is
 * tabbable but does not move focus on its own — without this the arrow key
 * would change the selection under a focus ring that stayed put.
 */
export function focusTabAt(container: HTMLElement | null | undefined, index: number): void {
  const tabs = container?.querySelectorAll<HTMLElement>('[role="tab"]');
  tabs?.[index]?.focus();
}
