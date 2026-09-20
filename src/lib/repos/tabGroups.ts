import { normalizeRepoPath, pathSegments } from "./paths";
import type { OpenRepoTab } from "../stores/repoStore";

export const MAX_GROUP_NAME_LENGTH = 40;
export const MAX_COLLAPSED_GROUPS = 64;

const CONTROL_CHARS = /[\u0000-\u001F\u007F]/;

/**
 * Normalizes a user-provided or auto-generated group name.
 * Trims whitespace, strips control characters, clamps to max length,
 * and returns null if the result is empty.
 */
export function normalizeGroupName(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.normalize("NFC").trim();
  if (!trimmed || CONTROL_CHARS.test(trimmed)) return null;
  const clamped = trimmed.slice(0, MAX_GROUP_NAME_LENGTH).trim();
  return clamped || null;
}

/**
 * Derives the parent folder name for a repository path.
 * For example:
 *   "/Users/developer/code/devtools/GitPulse" -> "devtools"
 *   "C:/Users/dev/repos/api" -> "repos"
 *   "/repo" -> null (no parent directory above root)
 */
export function parentFolderName(rawPath: string): string | null {
  const normalized = normalizeRepoPath(rawPath);
  if (!normalized) return null;
  const segments = pathSegments(normalized);
  if (segments.length < 2) return null;
  const parent = segments[segments.length - 2];
  return normalizeGroupName(parent);
}

export interface GroupHeaderItem {
  kind: "group-header";
  id: string;
  group: string;
  label: string;
  tabCount: number;
  isCollapsed: boolean;
  hasActiveTab: boolean;
  isDirty: boolean;
  conflictedCount: number;
  terminalCount: number;
  tabIds: string[];
}

export interface TabItem {
  kind: "tab";
  id: string;
  tab: OpenRepoTab;
  group: string | null;
  index: number;
  isInsideCollapsedGroup: boolean;
}

export type TabLayoutItem = GroupHeaderItem | TabItem;

export interface TabLayoutResult {
  /** All items in display order, including collapsed tab items (marked with isInsideCollapsedGroup). */
  allItems: TabLayoutItem[];
  /** Only the items that should be rendered on screen. */
  visibleItems: TabLayoutItem[];
  /** Distinct group headers. */
  groups: GroupHeaderItem[];
  /** Tabs that are currently visible (not in a collapsed group). */
  visibleTabs: OpenRepoTab[];
}

/**
 * Organizes open repository tabs into groups and calculates group headers.
 * Tabs belonging to the same group are grouped under their group header.
 * When a group is collapsed, its member tabs are hidden from visibleItems
 * while the group header remains visible, displaying counts and status badges.
 */
export function computeTabLayout(
  tabs: readonly OpenRepoTab[],
  collapsedGroups: Iterable<string> = [],
  terminalCounts?: Map<string, number>,
): TabLayoutResult {
  const collapsedSet = new Set(
    Array.from(collapsedGroups).map((g) => normalizeGroupName(g)).filter((g): g is string => g !== null),
  );

  // Group metadata maps
  const groupTabsMap = new Map<string, OpenRepoTab[]>();
  const groupFirstSeen = new Map<string, number>();

  // Determine groups and their first occurrence order
  tabs.forEach((tab, index) => {
    const group = normalizeGroupName(tab.group);
    if (group) {
      if (!groupTabsMap.has(group)) {
        groupTabsMap.set(group, []);
        groupFirstSeen.set(group, index);
      }
      groupTabsMap.get(group)!.push(tab);
    }
  });

  const groupHeaders = new Map<string, GroupHeaderItem>();
  for (const [group, groupTabs] of groupTabsMap.entries()) {
    const isCollapsed = collapsedSet.has(group);
    let hasActiveTab = false;
    let isDirty = false;
    let conflictedCount = 0;
    let terminalCount = 0;
    const tabIds: string[] = [];

    for (const t of groupTabs) {
      tabIds.push(t.id);
      if (t.isActive) hasActiveTab = true;
      if (t.isDirty) isDirty = true;
      conflictedCount += t.conflictedCount || 0;
      if (terminalCounts) {
        terminalCount += terminalCounts.get(t.path) || 0;
      }
    }

    groupHeaders.set(group, {
      kind: "group-header",
      id: `group:${group}`,
      group,
      label: group,
      tabCount: groupTabs.length,
      isCollapsed,
      hasActiveTab,
      isDirty,
      conflictedCount,
      terminalCount,
      tabIds,
    });
  }

  const allItems: TabLayoutItem[] = [];
  const visibleItems: TabLayoutItem[] = [];
  const visibleTabs: OpenRepoTab[] = [];
  const renderedGroups = new Set<string>();

  tabs.forEach((tab, originalIndex) => {
    const group = normalizeGroupName(tab.group);
    if (!group) {
      // Ungrouped tab
      const tabItem: TabItem = {
        kind: "tab",
        id: tab.id,
        tab,
        group: null,
        index: originalIndex,
        isInsideCollapsedGroup: false,
      };
      allItems.push(tabItem);
      visibleItems.push(tabItem);
      visibleTabs.push(tab);
      return;
    }

    // If this is the first time we encounter this group, emit the group header and all its tabs
    if (!renderedGroups.has(group)) {
      renderedGroups.add(group);
      const header = groupHeaders.get(group)!;
      allItems.push(header);
      visibleItems.push(header);

      const memberTabs = groupTabsMap.get(group) || [];
      for (const memberTab of memberTabs) {
        const memberOriginalIdx = tabs.findIndex((t) => t.id === memberTab.id);
        const tabItem: TabItem = {
          kind: "tab",
          id: memberTab.id,
          tab: memberTab,
          group,
          index: memberOriginalIdx >= 0 ? memberOriginalIdx : originalIndex,
          isInsideCollapsedGroup: header.isCollapsed,
        };
        allItems.push(tabItem);
        if (!header.isCollapsed) {
          visibleItems.push(tabItem);
          visibleTabs.push(memberTab);
        }
      }
    }
  });

  return {
    allItems,
    visibleItems,
    groups: Array.from(groupHeaders.values()),
    visibleTabs,
  };
}
