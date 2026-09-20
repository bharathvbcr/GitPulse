import {
  identityKey,
  normalizeRepoPath,
  type PathIdentityOptions,
} from "./paths";
import {
  MAX_COLLAPSED_GROUPS,
  normalizeGroupName,
  parentFolderName,
} from "./tabGroups";

export const MAX_OPEN_TABS = 24;
export const MAX_RECENT_REPOS = 24;
export const MAX_LAST_CLOSED = 16;

export type CloseTabReason = "missing" | "ok";

export interface TabRecord {
  id: string;
  path: string;
  pinned: boolean;
  group?: string | null;
}

export interface WorkspaceTabs {
  tabs: TabRecord[];
  activeId: string | null;
  recents: string[];
  lastClosed: string[];
  collapsedGroups?: string[];
}

export type OpenTabResult =
  | { ok: true; workspace: WorkspaceTabs; id: string; created: boolean }
  | { ok: false; reason: "invalid" | "capacity"; workspace: WorkspaceTabs };

export function emptyWorkspace(): WorkspaceTabs {
  return { tabs: [], activeId: null, recents: [], lastClosed: [], collapsedGroups: [] };
}

export function assertWorkspaceInvariants(ws: WorkspaceTabs, options: PathIdentityOptions): void {
  const ids = new Set<string>();
  for (const tab of ws.tabs) {
    if (!tab.id || !tab.path) {
      throw new Error("tab missing id or path");
    }
    if (ids.has(tab.id)) {
      throw new Error(`duplicate tab id ${tab.id}`);
    }
    ids.add(tab.id);
    if (identityKey(tab.path, options) !== tab.id) {
      throw new Error(`tab id does not match path identity: ${tab.id}`);
    }
    if (tab.group !== undefined && tab.group !== null && typeof tab.group !== "string") {
      throw new Error(`tab ${tab.id} has invalid group type`);
    }
  }
  if (ws.activeId === null) {
    if (ws.tabs.length !== 0) {
      throw new Error("activeId is null while tabs remain");
    }
  } else if (!ws.tabs.some((tab) => tab.id === ws.activeId)) {
    throw new Error("activeId is not in the tab list");
  }
  if (ws.recents.length > MAX_RECENT_REPOS) {
    throw new Error("recents exceeded cap");
  }
  if (ws.lastClosed.length > MAX_LAST_CLOSED) {
    throw new Error("lastClosed exceeded cap");
  }
  if (ws.collapsedGroups && !Array.isArray(ws.collapsedGroups)) {
    throw new Error("collapsedGroups is not an array");
  }
}

function cleanCollapsedGroups(tabs: TabRecord[], collapsed: string[] = []): string[] {
  if (collapsed.length === 0) return [];
  const activeGroups = new Set<string>();
  for (const t of tabs) {
    if (t.group) activeGroups.add(t.group);
  }
  return collapsed.filter((g) => activeGroups.has(g));
}

function expandGroupForTab(ws: WorkspaceTabs, tabId: string | null): string[] {
  const collapsed = ws.collapsedGroups ?? [];
  if (!tabId || collapsed.length === 0) return collapsed;
  const tab = ws.tabs.find((t) => t.id === tabId);
  if (!tab?.group || !collapsed.includes(tab.group)) return collapsed;
  return collapsed.filter((g) => g !== tab.group);
}

export function openTab(
  ws: WorkspaceTabs,
  rawPath: string,
  options: PathIdentityOptions,
  extras: { pinned?: boolean; activate?: boolean; group?: string | null } = {},
): OpenTabResult {
  const normalized = normalizeRepoPath(rawPath);
  if (!normalized) {
    return { ok: false, reason: "invalid", workspace: ws };
  }
  const id = identityKey(normalized, options);
  const shouldActivate = extras.activate !== false;
  const existing = ws.tabs.find((tab) => tab.id === id);
  if (existing) {
    const tabGroup = extras.group !== undefined
      ? normalizeGroupName(extras.group)
      : (existing.group ?? null);
    const tabs = ws.tabs.map((tab) =>
      tab.id === id
        ? {
            ...tab,
            path: normalized,
            pinned: extras.pinned ?? tab.pinned,
            group: tabGroup,
          }
        : tab,
    );
    let collapsedGroups = cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []);
    if (shouldActivate && tabGroup && collapsedGroups.includes(tabGroup)) {
      collapsedGroups = collapsedGroups.filter((g) => g !== tabGroup);
    }
    return {
      ok: true,
      created: false,
      id,
      workspace: rememberRecent(
        { ...ws, tabs, activeId: shouldActivate ? id : ws.activeId, collapsedGroups },
        normalized,
        options,
      ),
    };
  }
  if (ws.tabs.length >= MAX_OPEN_TABS) {
    return { ok: false, reason: "capacity", workspace: ws };
  }
  const tabGroup = normalizeGroupName(extras.group);
  const tab: TabRecord = {
    id,
    path: normalized,
    pinned: extras.pinned === true,
    group: tabGroup,
  };
  let collapsedGroups = cleanCollapsedGroups([...ws.tabs, tab], ws.collapsedGroups ?? []);
  if (shouldActivate && tabGroup && collapsedGroups.includes(tabGroup)) {
    collapsedGroups = collapsedGroups.filter((g) => g !== tabGroup);
  }
  return {
    ok: true,
    created: true,
    id,
    workspace: rememberRecent(
      {
        ...ws,
        tabs: [...ws.tabs, tab],
        activeId: shouldActivate ? id : ws.activeId ?? id,
        collapsedGroups,
      },
      normalized,
      options,
    ),
  };
}

export function closeTab(
  ws: WorkspaceTabs,
  id: string,
): { workspace: WorkspaceTabs; closedPath: string | null; reason: CloseTabReason } {
  const index = ws.tabs.findIndex((tab) => tab.id === id);
  if (index < 0) {
    return { workspace: ws, closedPath: null, reason: "missing" };
  }
  const closed = ws.tabs[index];
  const tabs = ws.tabs.filter((tab) => tab.id !== id);
  let activeId = ws.activeId;
  if (ws.activeId === id) {
    const neighbor = tabs[index] ?? tabs[index - 1] ?? null;
    activeId = neighbor?.id ?? null;
  }
  const lastClosed = pushFrontUnique(ws.lastClosed, closed.path, MAX_LAST_CLOSED);
  const collapsedGroups = cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []);
  return {
    reason: "ok",
    closedPath: closed.path,
    workspace: { ...ws, tabs, activeId, lastClosed, collapsedGroups },
  };
}

export function closeOtherTabs(ws: WorkspaceTabs, keepId: string): WorkspaceTabs {
  const keep = ws.tabs.find((tab) => tab.id === keepId);
  if (!keep) return ws;
  const closed = ws.tabs.filter((tab) => tab.id !== keepId).map((tab) => tab.path);
  let lastClosed = ws.lastClosed;
  // Left-to-right close so lastClosed[0] is the rightmost (most recently closed) tab.
  for (const path of closed) {
    lastClosed = pushFrontUnique(lastClosed, path, MAX_LAST_CLOSED);
  }
  const collapsedGroups = cleanCollapsedGroups([keep], ws.collapsedGroups ?? []);
  return { ...ws, tabs: [keep], activeId: keep.id, lastClosed, collapsedGroups };
}

export function closeTabsToTheRight(ws: WorkspaceTabs, id: string): WorkspaceTabs {
  const index = ws.tabs.findIndex((tab) => tab.id === id);
  if (index < 0) return ws;
  const removed = ws.tabs.slice(index + 1);
  if (removed.length === 0) return ws;
  const tabs = ws.tabs.slice(0, index + 1);
  let lastClosed = ws.lastClosed;
  for (const tab of removed) {
    lastClosed = pushFrontUnique(lastClosed, tab.path, MAX_LAST_CLOSED);
  }
  const activeStillOpen = tabs.some((tab) => tab.id === ws.activeId);
  const collapsedGroups = cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []);
  return {
    ...ws,
    tabs,
    lastClosed,
    activeId: activeStillOpen ? ws.activeId : id,
    collapsedGroups,
  };
}

export function activateTab(ws: WorkspaceTabs, id: string): WorkspaceTabs {
  if (!ws.tabs.some((tab) => tab.id === id)) return ws;
  const collapsedGroups = expandGroupForTab(ws, id);
  if (ws.activeId === id && collapsedGroups.length === (ws.collapsedGroups ?? []).length) return ws;
  return { ...ws, activeId: id, collapsedGroups };
}

export function activateAt(ws: WorkspaceTabs, index: number): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const clamped = Math.max(0, Math.min(index, ws.tabs.length - 1));
  const targetId = ws.tabs[clamped].id;
  const collapsedGroups = expandGroupForTab(ws, targetId);
  return { ...ws, activeId: targetId, collapsedGroups };
}

export function activateNext(ws: WorkspaceTabs): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const index = Math.max(0, ws.tabs.findIndex((tab) => tab.id === ws.activeId));
  const next = (index + 1) % ws.tabs.length;
  const targetId = ws.tabs[next].id;
  const collapsedGroups = expandGroupForTab(ws, targetId);
  return { ...ws, activeId: targetId, collapsedGroups };
}

export function activatePrev(ws: WorkspaceTabs): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const index = Math.max(0, ws.tabs.findIndex((tab) => tab.id === ws.activeId));
  const prev = (index - 1 + ws.tabs.length) % ws.tabs.length;
  const targetId = ws.tabs[prev].id;
  const collapsedGroups = expandGroupForTab(ws, targetId);
  return { ...ws, activeId: targetId, collapsedGroups };
}

export function reorderTab(ws: WorkspaceTabs, fromIndex: number, toIndex: number): WorkspaceTabs {
  if (
    fromIndex === toIndex ||
    fromIndex < 0 ||
    toIndex < 0 ||
    fromIndex >= ws.tabs.length ||
    toIndex >= ws.tabs.length
  ) {
    return ws;
  }
  const tabs = [...ws.tabs];
  const [moved] = tabs.splice(fromIndex, 1);
  tabs.splice(toIndex, 0, moved);
  return { ...ws, tabs };
}

/**
 * Moves the tab with `id` to `toIndex`. Unknown ids and out-of-range
 * destinations are no-ops so a stale menu or drag cannot shuffle the strip.
 */
export function moveTabTo(ws: WorkspaceTabs, id: string, toIndex: number): WorkspaceTabs {
  const fromIndex = ws.tabs.findIndex((tab) => tab.id === id);
  if (fromIndex < 0) return ws;
  return reorderTab(ws, fromIndex, toIndex);
}

/** Adjacent step. Past-the-end deltas are no-ops, same as `reorderTab`. */
export function moveTabBy(ws: WorkspaceTabs, id: string, delta: number): WorkspaceTabs {
  const fromIndex = ws.tabs.findIndex((tab) => tab.id === id);
  if (fromIndex < 0 || delta === 0 || !Number.isInteger(delta)) return ws;
  return reorderTab(ws, fromIndex, fromIndex + delta);
}

/**
 * Drop onto a tab's left (`before`) or right half: the index `reorderTab`
 * should receive, or null when the drop would not change order (self or the
 * immediate neighbor gap).
 */
export function dropReorderIndex(
  fromIndex: number,
  targetIndex: number,
  before: boolean,
): number | null {
  if (
    fromIndex < 0 ||
    targetIndex < 0 ||
    !Number.isInteger(fromIndex) ||
    !Number.isInteger(targetIndex)
  ) {
    return null;
  }
  const insertAt = before ? targetIndex : targetIndex + 1;
  if (insertAt === fromIndex || insertAt === fromIndex + 1) return null;
  return insertAt > fromIndex ? insertAt - 1 : insertAt;
}

export function pinTab(ws: WorkspaceTabs, id: string, pinned: boolean): WorkspaceTabs {
  if (!ws.tabs.some((tab) => tab.id === id)) return ws;
  return {
    ...ws,
    tabs: ws.tabs.map((tab) => (tab.id === id ? { ...tab, pinned } : tab)),
  };
}

export function rememberRecent(
  ws: WorkspaceTabs,
  rawPath: string,
  options: PathIdentityOptions,
): WorkspaceTabs {
  const normalized = normalizeRepoPath(rawPath);
  if (!normalized) return ws;
  const recents = [
    normalized,
    ...ws.recents.filter((path) => !sameIdentity(path, normalized, options)),
  ].slice(0, MAX_RECENT_REPOS);
  return { ...ws, recents };
}

export function removeRecent(
  ws: WorkspaceTabs,
  rawPath: string,
  options: PathIdentityOptions,
): WorkspaceTabs {
  const recents = ws.recents.filter((path) => !sameIdentity(path, rawPath, options));
  return { ...ws, recents };
}

export function reopenLastClosed(
  ws: WorkspaceTabs,
  options: PathIdentityOptions,
): OpenTabResult {
  const next = ws.lastClosed[0];
  if (!next) {
    return { ok: false, reason: "invalid", workspace: ws };
  }
  const without = { ...ws, lastClosed: ws.lastClosed.slice(1) };
  return openTab(without, next, options);
}

function sameIdentity(a: string, b: string, options: PathIdentityOptions): boolean {
  const left = identityKey(a, options);
  const right = identityKey(b, options);
  return Boolean(left) && left === right;
}

function pushFrontUnique(list: string[], value: string, cap: number): string[] {
  return [value, ...list.filter((item) => item !== value)].slice(0, cap);
}

/**
 * Assigns or clears a group for the given tab id.
 */
export function setTabGroup(ws: WorkspaceTabs, id: string, rawGroup: string | null): WorkspaceTabs {
  const group = normalizeGroupName(rawGroup);
  if (!ws.tabs.some((t) => t.id === id)) return ws;
  const tabs = ws.tabs.map((tab) => (tab.id === id ? { ...tab, group } : tab));
  return {
    ...ws,
    tabs,
    collapsedGroups: cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []),
  };
}

/**
 * Sets a group's collapsed state.
 */
export function setGroupCollapsed(
  ws: WorkspaceTabs,
  rawGroup: string,
  collapsed: boolean,
): WorkspaceTabs {
  const group = normalizeGroupName(rawGroup);
  if (!group) return ws;
  const current = ws.collapsedGroups ?? [];
  const isCollapsed = current.includes(group);
  if (collapsed === isCollapsed) return ws;
  const next = collapsed
    ? [...current, group].slice(0, MAX_COLLAPSED_GROUPS)
    : current.filter((g) => g !== group);
  return {
    ...ws,
    collapsedGroups: cleanCollapsedGroups(ws.tabs, next),
  };
}

/**
 * Toggles a group's collapsed state.
 */
export function toggleGroupCollapsed(ws: WorkspaceTabs, rawGroup: string): WorkspaceTabs {
  const group = normalizeGroupName(rawGroup);
  if (!group) return ws;
  const current = ws.collapsedGroups ?? [];
  const isCollapsed = current.includes(group);
  return setGroupCollapsed(ws, group, !isCollapsed);
}

/**
 * Returns whether a group is currently collapsed.
 */
export function isGroupCollapsed(ws: WorkspaceTabs, rawGroup: string): boolean {
  const group = normalizeGroupName(rawGroup);
  if (!group) return false;
  return (ws.collapsedGroups ?? []).includes(group);
}

/**
 * Renames all tabs in oldGroup to newGroup.
 */
export function renameGroup(ws: WorkspaceTabs, rawOld: string, rawNew: string): WorkspaceTabs {
  const oldGroup = normalizeGroupName(rawOld);
  const newGroup = normalizeGroupName(rawNew);
  if (!oldGroup || !newGroup || oldGroup === newGroup) return ws;
  const tabs = ws.tabs.map((tab) => (tab.group === oldGroup ? { ...tab, group: newGroup } : tab));
  const collapsedGroups = (ws.collapsedGroups ?? []).map((g) => (g === oldGroup ? newGroup : g));
  return {
    ...ws,
    tabs,
    collapsedGroups: cleanCollapsedGroups(tabs, Array.from(new Set(collapsedGroups))),
  };
}

/**
 * Ungroups tabs (all tabs, or all tabs in a specific group).
 */
export function ungroupTabs(ws: WorkspaceTabs, rawGroup?: string): WorkspaceTabs {
  const targetGroup = rawGroup ? normalizeGroupName(rawGroup) : null;
  const tabs = ws.tabs.map((tab) => {
    if (targetGroup) {
      return tab.group === targetGroup ? { ...tab, group: null } : tab;
    }
    return { ...tab, group: null };
  });
  return {
    ...ws,
    tabs,
    collapsedGroups: cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []),
  };
}

/**
 * Closes all tabs in a specific group.
 */
export function closeGroup(
  ws: WorkspaceTabs,
  rawGroup: string,
): { workspace: WorkspaceTabs; closedPaths: string[] } {
  const group = normalizeGroupName(rawGroup);
  if (!group) return { workspace: ws, closedPaths: [] };
  const toClose = ws.tabs.filter((t) => t.group === group);
  if (toClose.length === 0) return { workspace: ws, closedPaths: [] };
  let currentWs = ws;
  const closedPaths: string[] = [];
  for (const tab of toClose) {
    const res = closeTab(currentWs, tab.id);
    currentWs = res.workspace;
    if (res.closedPath) closedPaths.push(res.closedPath);
  }
  return { workspace: currentWs, closedPaths };
}

/**
 * Automatically groups open tabs by their parent folder name.
 * Tabs in the same parent folder are clustered together.
 */
export function groupByParentFolder(ws: WorkspaceTabs): WorkspaceTabs {
  const mapped = ws.tabs.map((tab) => {
    const parent = parentFolderName(tab.path);
    return {
      ...tab,
      group: parent ?? null,
    };
  });

  // Cluster tabs by group while preserving initial order
  const clustered: TabRecord[] = [];
  const processedGroups = new Set<string>();

  for (const tab of mapped) {
    if (!tab.group) {
      clustered.push(tab);
      continue;
    }
    if (processedGroups.has(tab.group)) continue;
    processedGroups.add(tab.group);
    const members = mapped.filter((t) => t.group === tab.group);
    clustered.push(...members);
  }

  return {
    ...ws,
    tabs: clustered,
    collapsedGroups: cleanCollapsedGroups(clustered, ws.collapsedGroups ?? []),
  };
}

/**
 * Collapses all distinct groups currently present in open tabs.
 */
export function collapseAllGroups(ws: WorkspaceTabs): WorkspaceTabs {
  const groups = new Set<string>();
  for (const tab of ws.tabs) {
    if (tab.group) groups.add(tab.group);
  }
  const collapsedGroups = cleanCollapsedGroups(ws.tabs, Array.from(groups));
  return { ...ws, collapsedGroups };
}

/**
 * Expands all groups currently collapsed in the workspace.
 */
export function expandAllGroups(ws: WorkspaceTabs): WorkspaceTabs {
  if (!ws.collapsedGroups || ws.collapsedGroups.length === 0) return ws;
  return { ...ws, collapsedGroups: [] };
}


