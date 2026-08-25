import { VIEW_TABS, type ViewTab } from "../repos/persist";
import { REGISTERED_VIEWS, type ViewGroupId } from "./viewRegistry";

export type { ViewGroupId };
export type ViewNavKind = "tabs" | "menu";

export interface ViewNavItem {
  id: ViewTab;
  label: string;
}

export interface ViewNavGroup {
  id: ViewGroupId;
  label: string;
  kind: ViewNavKind;
  items: readonly ViewNavItem[];
}

/**
 * Header view catalog, derived from the view registry: daily work stays as
 * tabs; the rest folds into menus so the title bar cannot grow a new button
 * for every panel. Registering a view with a menuGroup is all it takes to
 * place it here.
 */
const NAV_GROUP_LAYOUT: readonly Pick<ViewNavGroup, "id" | "label" | "kind">[] = [
  { id: "work", label: "Work", kind: "tabs" },
  { id: "inspect", label: "Inspect", kind: "menu" },
  { id: "more", label: "More", kind: "menu" },
];

export const VIEW_NAV: readonly ViewNavGroup[] = NAV_GROUP_LAYOUT.map((layout) => ({
  ...layout,
  items: REGISTERED_VIEWS.filter((view) => view.menuGroup === layout.id).map((view) => ({
    id: view.id,
    label: view.label,
  })),
}));

export function flattenedViewNavTabs(groups: readonly ViewNavGroup[] = VIEW_NAV): ViewTab[] {
  return groups.flatMap((group) => group.items.map((item) => item.id));
}

export function viewNavPartitionsAllTabs(groups: readonly ViewNavGroup[] = VIEW_NAV): boolean {
  const ids = flattenedViewNavTabs(groups);
  if (ids.length !== VIEW_TABS.length) return false;
  const unique = new Set(ids);
  if (unique.size !== ids.length) return false;
  return VIEW_TABS.every((tab) => unique.has(tab));
}

export function viewNavGroupFor(
  tab: ViewTab,
  groups: readonly ViewNavGroup[] = VIEW_NAV,
): ViewNavGroup | undefined {
  return groups.find((group) => group.items.some((item) => item.id === tab));
}

export function viewNavItemFor(
  tab: ViewTab,
  groups: readonly ViewNavGroup[] = VIEW_NAV,
): ViewNavItem | undefined {
  for (const group of groups) {
    const item = group.items.find((entry) => entry.id === tab);
    if (item) return item;
  }
  return undefined;
}

export function isViewNavGroupActive(group: ViewNavGroup, tab: ViewTab): boolean {
  return group.items.some((item) => item.id === tab);
}

export function viewNavTriggerLabel(group: ViewNavGroup, tab: ViewTab): string {
  const active = group.items.find((item) => item.id === tab);
  return active?.label ?? group.label;
}

export function formatViewTabLabel(item: ViewNavItem, conflictedCount = 0): string {
  if (item.id !== "conflict" || conflictedCount <= 0) return item.label;
  return `${item.label} (${conflictedCount})`;
}
