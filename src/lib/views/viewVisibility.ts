import { isViewTab, type ViewTab } from "../repos/persist";
import { VIEW_NAV, type ViewNavItem } from "./viewNav";

/**
 * Which views the header nav lists.
 *
 * The header is a shortcut surface, not the only door: a view hidden here is
 * still reachable from the command palette and the native View menu, and its
 * pane still renders when something switches to it. Hiding is therefore about
 * noise, never about capability — which is why `pinnedVisibleReason` exists.
 */
export interface ViewVisibilityContext {
  /** The view currently on screen. */
  activeTab: ViewTab;
  /** Files still carrying conflict markers. */
  conflictedCount?: number;
}

/** Persisted hidden lists are user data: drop anything that is not a view. */
export function sanitizeHiddenViews(value: unknown): ViewTab[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<ViewTab>();
  for (const entry of value) {
    if (isViewTab(entry)) seen.add(entry);
  }
  return [...seen];
}

/**
 * Why a view is on screen despite being hidden, or null when the preference
 * applies.
 *
 * Two overrides, both about not lying to the user:
 *
 * - the active view, so the header always says where you are; and
 * - Work while conflicts are unresolved, so a cleaner header can never be the
 *   reason a parked merge goes unnoticed.
 *
 * The second override used to name Resolve, which was its own view. Resolve
 * is a section of Work now, so the pin follows the content: hiding Work with
 * conflicts outstanding would take away the door to the editor that fixes
 * them. Work also carries the count in its section bar, its rows sort blocked
 * worktrees first, and the status bar keeps an independent chip — the
 * guarantee is stronger than it was, not weaker.
 */
export function pinnedVisibleReason(
  id: ViewTab,
  context: ViewVisibilityContext,
): "active" | "conflicts" | null {
  if (id === context.activeTab) return "active";
  if (id === "work" && (context.conflictedCount ?? 0) > 0) return "conflicts";
  return null;
}

export function isViewVisible(
  id: ViewTab,
  hidden: readonly ViewTab[],
  context: ViewVisibilityContext,
): boolean {
  if (pinnedVisibleReason(id, context)) return true;
  return !hidden.includes(id);
}

/** The header list with hidden views removed, in registry order. */
export function visibleViewNav(
  hidden: readonly ViewTab[],
  context: ViewVisibilityContext,
  items: readonly ViewNavItem[] = VIEW_NAV,
): ViewNavItem[] {
  return items.filter((item) => isViewVisible(item.id, hidden, context));
}
