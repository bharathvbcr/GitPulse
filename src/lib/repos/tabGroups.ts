import { normalizeRepoPath, pathSegments } from "./paths";
import { familyLabels, familyName } from "./repoFamily";
import type { OpenRepoTab } from "../stores/repoStore";
import { lookupGroupColor, type GroupColor, type TabColor } from "./tabColors";

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
  color: TabColor | null;
}

/**
 * Several checkouts of one repository, drawn as one header.
 *
 * Each worktree used to cost a whole tab, so a repository with five agent
 * worktrees took six headers and pushed every other repository off the strip.
 * A stack is keyed by (user group, repository family): a person's own grouping
 * always wins, and a family split across two groups stacks once in each.
 */
export interface StackHeaderItem {
  kind: "stack-header";
  /** `stack:` + key; unique among layout items. */
  id: string;
  key: string;
  root: string;
  label: string;
  group: string | null;
  /** Members in strip order. Always two or more. */
  tabIds: string[];
  tabCount: number;
  /**
   * The member the header stands for: the active tab when it is a member,
   * else the one last used, else the primary checkout, else the first.
   */
  current: OpenRepoTab;
  isExpanded: boolean;
  isInsideCollapsedGroup: boolean;
  hasActiveTab: boolean;
  isDirty: boolean;
  conflictedCount: number;
  terminalCount: number;
}

export interface TabItem {
  kind: "tab";
  id: string;
  tab: OpenRepoTab;
  group: string | null;
  index: number;
  isInsideCollapsedGroup: boolean;
  /** Key of the stack this tab belongs to, or null. */
  stack: string | null;
}

export type TabLayoutItem = GroupHeaderItem | StackHeaderItem | TabItem;

/** The strip as a tree: moves and drops happen among siblings. */
export type StripNode =
  | { kind: "tab"; item: TabItem }
  | { kind: "stack"; header: StackHeaderItem; members: TabItem[]; children: StripNode[] }
  | { kind: "group"; header: GroupHeaderItem; children: StripNode[] };

export interface StackingOptions {
  enabled: boolean;
  /** Stack keys the reader has unfolded. */
  expanded?: Iterable<string>;
  /** Family key → the checkout last shown, for headers whose family is not active. */
  lastUsed?: ReadonlyMap<string, string>;
  /** Path identity, so "is this the primary checkout" survives case folding. */
  identity?: (path: string) => string;
}

export interface TabLayoutResult {
  /** All items in display order, including collapsed tab items (marked with isInsideCollapsedGroup). */
  allItems: TabLayoutItem[];
  /** Only the items that should be rendered on screen. */
  visibleItems: TabLayoutItem[];
  /** Distinct group headers. */
  groups: GroupHeaderItem[];
  /** Distinct worktree stacks. */
  stacks: StackHeaderItem[];
  /** Tabs drawn as their own pill (not in a collapsed group or folded stack). */
  visibleTabs: OpenRepoTab[];
  /** Top-level nodes in display order. */
  tree: StripNode[];
  /** Every tab id once, in display order — what "the order on screen" means. */
  order: string[];
}

/** Separator for stack keys: group names and paths both refuse control characters. */
const STACK_KEY_SEPARATOR = "\u0001";

export function stackKeyFor(group: string | null, family: string): string {
  return `${group ?? ""}${STACK_KEY_SEPARATOR}${family}`;
}

/**
 * Organizes open repository tabs into groups and worktree stacks.
 * Tabs belonging to the same group are drawn together under their group
 * header, at the position of the group's first member; checkouts of one
 * repository within one container are drawn together under one stack header.
 * When a group is collapsed, its member tabs are hidden from visibleItems
 * while the group header remains visible, displaying counts and status badges.
 */
export function computeTabLayout(
  tabs: readonly OpenRepoTab[],
  collapsedGroups: Iterable<string> = [],
  terminalCounts?: Map<string, number>,
  groupColors: readonly GroupColor[] = [],
  stacking: StackingOptions = { enabled: false },
): TabLayoutResult {
  const collapsedSet = new Set(
    Array.from(collapsedGroups).map((g) => normalizeGroupName(g)).filter((g): g is string => g !== null),
  );
  const expandedStacks = new Set(stacking.expanded ?? []);
  const identity = stacking.identity ?? ((path: string) => normalizeRepoPath(path) ?? path);

  const stackKeyOf = (tab: OpenRepoTab, group: string | null): string | null => {
    if (!stacking.enabled) return null;
    const family = typeof tab.family === "string" && tab.family.length > 0 ? tab.family : null;
    return family ? stackKeyFor(group, family) : null;
  };

  // A stack needs two checkouts in one container; a lone worktree is a tab.
  const stackSizes = new Map<string, number>();
  const groupTabsMap = new Map<string, OpenRepoTab[]>();
  for (const tab of tabs) {
    const group = normalizeGroupName(tab.group);
    if (group) {
      const members = groupTabsMap.get(group);
      if (members) members.push(tab);
      else groupTabsMap.set(group, [tab]);
    }
    const key = stackKeyOf(tab, group);
    if (key) stackSizes.set(key, (stackSizes.get(key) ?? 0) + 1);
  }

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
      color: lookupGroupColor(groupColors, group),
    });
  }

  // Build the tree in first-appearance order: a group sits where its first
  // member was, and a stack where its first checkout was.
  interface PendingStack {
    kind: "stack";
    key: string;
    group: string | null;
    family: string;
    root: string;
    members: Array<{ tab: OpenRepoTab; index: number }>;
  }
  interface PendingGroup {
    kind: "group";
    group: string;
    children: PendingNode[];
  }
  type PendingNode =
    | { kind: "tab"; tab: OpenRepoTab; index: number; group: string | null }
    | PendingStack
    | PendingGroup;

  const pendingTop: PendingNode[] = [];
  const pendingGroups = new Map<string, PendingGroup>();
  const pendingStacks = new Map<string, PendingStack>();
  tabs.forEach((tab, index) => {
    const group = normalizeGroupName(tab.group);
    let container = pendingTop;
    if (group) {
      let node = pendingGroups.get(group);
      if (!node) {
        node = { kind: "group", group, children: [] };
        pendingGroups.set(group, node);
        pendingTop.push(node);
      }
      container = node.children;
    }
    const key = stackKeyOf(tab, group);
    if (key && (stackSizes.get(key) ?? 0) >= 2) {
      let stack = pendingStacks.get(key);
      if (!stack) {
        stack = {
          kind: "stack",
          key,
          group,
          family: tab.family as string,
          root: normalizeRepoPath(tab.familyRoot ?? "") ?? tab.path,
          members: [],
        };
        pendingStacks.set(key, stack);
        container.push(stack);
      }
      stack.members.push({ tab, index });
      return;
    }
    container.push({ kind: "tab", tab, index, group });
  });

  const labels = familyLabels(Array.from(pendingStacks.values(), (stack) => stack.root));

  const tabItem = (
    tab: OpenRepoTab,
    index: number,
    group: string | null,
    stack: string | null,
  ): TabItem => ({
    kind: "tab",
    id: tab.id,
    tab,
    group,
    index,
    isInsideCollapsedGroup: group !== null && collapsedSet.has(group),
    stack,
  });

  const stackHeaders: StackHeaderItem[] = [];
  const toNode = (pending: PendingNode): StripNode => {
    if (pending.kind === "tab") {
      return { kind: "tab", item: tabItem(pending.tab, pending.index, pending.group, null) };
    }
    if (pending.kind === "group") {
      return {
        kind: "group",
        header: groupHeaders.get(pending.group)!,
        children: pending.children.map(toNode),
      };
    }
    const { members, key, root } = pending;
    const lastUsedId = stacking.lastUsed?.get(pending.family);
    const rootIdentity = identity(root);
    const current =
      members.find((m) => m.tab.isActive) ??
      members.find((m) => m.tab.id === lastUsedId) ??
      members.find((m) => identity(m.tab.path) === rootIdentity) ??
      members[0];
    const isExpanded = expandedStacks.has(key);
    let isDirty = false;
    let conflictedCount = 0;
    let terminalCount = 0;
    for (const { tab } of members) {
      if (tab.isDirty) isDirty = true;
      conflictedCount += tab.conflictedCount || 0;
      terminalCount += terminalCounts?.get(tab.path) || 0;
    }
    const header: StackHeaderItem = {
      kind: "stack-header",
      id: `stack:${key}`,
      key,
      root,
      label: labels.get(root) ?? familyName(root),
      group: pending.group,
      tabIds: members.map((m) => m.tab.id),
      tabCount: members.length,
      current: current.tab,
      isExpanded,
      isInsideCollapsedGroup: pending.group !== null && collapsedSet.has(pending.group),
      hasActiveTab: members.some((m) => m.tab.isActive),
      isDirty,
      conflictedCount,
      terminalCount,
    };
    stackHeaders.push(header);
    const memberItems = members.map((m) => tabItem(m.tab, m.index, pending.group, key));
    return {
      kind: "stack",
      header,
      members: memberItems,
      children: memberItems.map((item) => ({ kind: "tab" as const, item })),
    };
  };
  const tree = pendingTop.map(toNode);

  const allItems: TabLayoutItem[] = [];
  const visibleItems: TabLayoutItem[] = [];
  const visibleTabs: OpenRepoTab[] = [];
  const emit = (node: StripNode, hidden: boolean) => {
    if (node.kind === "tab") {
      allItems.push(node.item);
      if (!hidden) {
        visibleItems.push(node.item);
        visibleTabs.push(node.item.tab);
      }
      return;
    }
    if (node.kind === "stack") {
      allItems.push(node.header);
      if (!hidden) visibleItems.push(node.header);
      for (const member of node.members) {
        allItems.push(member);
        if (!hidden && node.header.isExpanded) {
          visibleItems.push(member);
          visibleTabs.push(member.tab);
        }
      }
      return;
    }
    allItems.push(node.header);
    visibleItems.push(node.header);
    for (const child of node.children) emit(child, node.header.isCollapsed);
  };
  for (const node of tree) emit(node, false);

  return {
    allItems,
    visibleItems,
    groups: Array.from(groupHeaders.values()),
    stacks: stackHeaders,
    visibleTabs,
    tree,
    order: allItems.filter((item): item is TabItem => item.kind === "tab").map((item) => item.id),
  };
}
