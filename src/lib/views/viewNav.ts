import { VIEW_TABS, type ViewTab } from "../repos/persist";
import { REGISTERED_VIEWS } from "./viewRegistry";

export interface ViewNavItem {
  id: ViewTab;
  label: string;
}

/**
 * Header view catalog, derived from the view registry — a flat list, because
 * there are four views and every one of them is a tab.
 *
 * It used to be groups: a "Work" strip of tabs plus "Inspect" and "More"
 * dropdowns, because fifteen views could not fit a title bar and something
 * had to fold. The consolidation removed the pressure rather than managing
 * it — a new panel is a *section* of the view that owns its subject now, not
 * a fifth top-level entry — so the grouping layer had one group left and the
 * dropdown branch had become unreachable. Both are gone: the header is the
 * four views, in registry order.
 */
export const VIEW_NAV: readonly ViewNavItem[] = REGISTERED_VIEWS.map((view) => ({
  id: view.id,
  label: view.label,
}));

export function viewNavTabs(items: readonly ViewNavItem[] = VIEW_NAV): ViewTab[] {
  return items.map((item) => item.id);
}

/** True when the header lists every registered view exactly once. */
export function viewNavCoversAllTabs(items: readonly ViewNavItem[] = VIEW_NAV): boolean {
  const ids = viewNavTabs(items);
  if (ids.length !== VIEW_TABS.length) return false;
  const unique = new Set(ids);
  if (unique.size !== ids.length) return false;
  return VIEW_TABS.every((tab) => unique.has(tab));
}

export function viewNavItemFor(
  tab: ViewTab,
  items: readonly ViewNavItem[] = VIEW_NAV,
): ViewNavItem | undefined {
  return items.find((item) => item.id === tab);
}

/**
 * The header label, with the unresolved-conflict count on the tab that now
 * owns the resolver.
 *
 * The count used to hang off Resolve, which was its own view. Resolve is a
 * section of Work now, and the count moved with it rather than being dropped:
 * a merge parked mid-conflict has to be visible from the header, not only
 * after you open Work and read its section bar.
 */
export function formatViewTabLabel(item: ViewNavItem, conflictedCount = 0): string {
  if (item.id !== "work" || conflictedCount <= 0) return item.label;
  return `${item.label} (${conflictedCount})`;
}
