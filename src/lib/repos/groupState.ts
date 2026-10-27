/**
 * Session state of the strip's tab groups.
 *
 * Tracks which groups have their inactive tabs collapsed (or explicitly expanded)
 * during the current session.
 */
import { writable, type Readable } from "svelte/store";

export const MAX_REMEMBERED_GROUPS = 64;

const collapsedInactive = writable<ReadonlySet<string>>(new Set());
const explicitlyExpanded = writable<ReadonlySet<string>>(new Set());

/** Group names whose inactive tabs are collapsed to focus on active tabs. */
export const collapsedInactiveGroups: Readable<ReadonlySet<string>> = {
  subscribe: collapsedInactive.subscribe,
};

/** Group names whose tabs are all explicitly expanded even when global collapseInactive is on. */
export const explicitlyExpandedGroups: Readable<ReadonlySet<string>> = {
  subscribe: explicitlyExpanded.subscribe,
};

export function setGroupInactiveCollapsed(group: string, collapsed: boolean): void {
  const name = typeof group === "string" ? group.trim() : "";
  if (!name) return;
  collapsedInactive.update((current) => {
    if (current.has(name) === collapsed) return current;
    const next = new Set(current);
    if (collapsed) {
      next.add(name);
      while (next.size > MAX_REMEMBERED_GROUPS) {
        const oldest = next.values().next().value;
        if (oldest === undefined) break;
        next.delete(oldest);
      }
    } else {
      next.delete(name);
    }
    return next;
  });
  if (collapsed) {
    explicitlyExpanded.update((current) => {
      if (!current.has(name)) return current;
      const next = new Set(current);
      next.delete(name);
      return next;
    });
  }
}

export function setGroupExplicitlyExpanded(group: string, expanded: boolean): void {
  const name = typeof group === "string" ? group.trim() : "";
  if (!name) return;
  explicitlyExpanded.update((current) => {
    if (current.has(name) === expanded) return current;
    const next = new Set(current);
    if (expanded) {
      next.add(name);
      while (next.size > MAX_REMEMBERED_GROUPS) {
        const oldest = next.values().next().value;
        if (oldest === undefined) break;
        next.delete(oldest);
      }
    } else {
      next.delete(name);
    }
    return next;
  });
  if (expanded) {
    collapsedInactive.update((current) => {
      if (!current.has(name)) return current;
      const next = new Set(current);
      next.delete(name);
      return next;
    });
  }
}

export function toggleGroupInactiveCollapsed(group: string): void {
  const name = typeof group === "string" ? group.trim() : "";
  if (!name) return;
  let currently = false;
  const unsubscribe = collapsedInactive.subscribe((s) => {
    currently = s.has(name);
  });
  unsubscribe();
  setGroupInactiveCollapsed(name, !currently);
}

export function clearGroupState(): void {
  collapsedInactive.set(new Set());
  explicitlyExpanded.set(new Set());
}
