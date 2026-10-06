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
import {
  lookupGroupColor,
  normalizeTabColor,
  type GroupColor,
  type TabColor,
} from "./tabColors";

export const MAX_OPEN_TABS = 24;
export const MAX_RECENT_REPOS = 24;
export const MAX_LAST_CLOSED = 16;

export type CloseTabReason = "missing" | "ok";

export interface TabRecord {
  id: string;
  path: string;
  pinned: boolean;
  group?: string | null;
  /** Own color. Null means "none", which still inherits a group color when drawn. */
  color?: TabColor | null;
}

export interface WorkspaceTabs {
  tabs: TabRecord[];
  activeId: string | null;
  recents: string[];
  lastClosed: string[];
  collapsedGroups?: string[];
  /** One entry per live group. Absent and empty mean the same thing. */
  groupColors?: GroupColor[];
}

export type OpenTabResult =
  | { ok: true; workspace: WorkspaceTabs; id: string; created: boolean }
  | { ok: false; reason: "invalid" | "capacity"; workspace: WorkspaceTabs };

export function emptyWorkspace(): WorkspaceTabs {
  return { tabs: [], activeId: null, recents: [], lastClosed: [], collapsedGroups: [], groupColors: [] };
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
    if (tab.color != null && normalizeTabColor(tab.color) !== tab.color) {
      throw new Error(`tab ${tab.id} has invalid color`);
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
  if (ws.groupColors !== undefined) {
    if (!Array.isArray(ws.groupColors)) {
      throw new Error("groupColors is not an array");
    }
    const live = liveGroupNames(ws.tabs);
    const seen = new Set<string>();
    for (const entry of ws.groupColors) {
      if (!entry || normalizeGroupName(entry.group) !== entry.group) {
        throw new Error("groupColors has an invalid group");
      }
      if (normalizeTabColor(entry.color) !== entry.color) {
        throw new Error(`group ${entry.group} has an invalid color`);
      }
      if (seen.has(entry.group)) {
        throw new Error(`duplicate group color ${entry.group}`);
      }
      if (!live.has(entry.group)) {
        throw new Error(`orphan group color ${entry.group}`);
      }
      seen.add(entry.group);
    }
  }
}

function cleanCollapsedGroups(tabs: readonly TabRecord[], collapsed: readonly string[] = []): string[] {
  if (collapsed.length === 0) return [];
  const activeGroups = liveGroupNames(tabs);
  return collapsed.filter((g) => activeGroups.has(g));
}

function liveGroupNames(tabs: readonly TabRecord[]): Set<string> {
  const activeGroups = new Set<string>();
  for (const tab of tabs) {
    const group = normalizeGroupName(tab.group);
    if (group) activeGroups.add(group);
  }
  return activeGroups;
}

function cleanGroupColors(
  tabs: readonly TabRecord[],
  colors: readonly GroupColor[] | undefined,
): GroupColor[] {
  if (!colors || colors.length === 0) return [];
  const live = liveGroupNames(tabs);
  const out: GroupColor[] = [];
  const seen = new Set<string>();
  for (const entry of colors) {
    if (!entry || typeof entry !== "object") continue;
    const group = normalizeGroupName(entry.group);
    const color = normalizeTabColor(entry.color);
    if (!group || !color || !live.has(group) || seen.has(group)) continue;
    seen.add(group);
    out.push({ group, color });
    if (out.length >= MAX_OPEN_TABS) break;
  }
  return out;
}

function withGroupState(
  tabs: readonly TabRecord[],
  collapsed: readonly string[],
  colors: readonly GroupColor[] | undefined,
): { collapsedGroups: string[]; groupColors: GroupColor[] } {
  return {
    collapsedGroups: cleanCollapsedGroups(tabs, collapsed),
    groupColors: cleanGroupColors(tabs, colors),
  };
}

/**
 * Null and "" clear. A palette name sets. Anything else is refused so a bad
 * caller cannot wipe a color it did not mean to clear.
 */
function interpretColorEdit(raw: unknown): TabColor | null | undefined {
  if (raw == null || raw === "") return null;
  return normalizeTabColor(raw) ?? undefined;
}

function resolveColor(
  current: TabColor | null | undefined,
  extra: unknown,
  provided: boolean,
): TabColor | null {
  const kept = normalizeTabColor(current) ?? null;
  if (!provided) return kept;
  const edited = interpretColorEdit(extra);
  return edited === undefined ? kept : edited;
}

function renameGroupColors(
  colors: readonly GroupColor[] | undefined,
  oldGroup: string,
  newGroup: string,
): GroupColor[] {
  const list = colors ?? [];
  const source = list.find((entry) => entry.group === oldGroup);
  const destination = list.find((entry) => entry.group === newGroup);
  const kept = list.filter((entry) => entry.group !== oldGroup);
  if (!source || destination) return kept;
  return [...kept, { group: newGroup, color: source.color }];
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
  extras: { pinned?: boolean; activate?: boolean; group?: string | null; color?: TabColor | null } = {},
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
    const colorProvided = extras.color !== undefined;
    const tabs = ws.tabs.map((tab) =>
      tab.id === id
        ? {
            ...tab,
            path: normalized,
            pinned: extras.pinned ?? tab.pinned,
            group: tabGroup,
            color: resolveColor(tab.color, extras.color, colorProvided),
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
        {
          ...ws,
          tabs,
          activeId: shouldActivate ? id : ws.activeId,
          ...withGroupState(tabs, collapsedGroups, ws.groupColors),
        },
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
    color: resolveColor(null, extras.color, extras.color !== undefined),
  };
  const tabs = [...ws.tabs, tab];
  let collapsedGroups = cleanCollapsedGroups(tabs, ws.collapsedGroups ?? []);
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
        tabs,
        activeId: shouldActivate ? id : ws.activeId ?? id,
        ...withGroupState(tabs, collapsedGroups, ws.groupColors),
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
  return {
    reason: "ok",
    closedPath: closed.path,
    workspace: {
      ...ws,
      tabs,
      activeId,
      lastClosed,
      ...withGroupState(tabs, ws.collapsedGroups ?? [], ws.groupColors),
    },
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
  return {
    ...ws,
    tabs: [keep],
    activeId: keep.id,
    lastClosed,
    ...withGroupState([keep], ws.collapsedGroups ?? [], ws.groupColors),
  };
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
  return {
    ...ws,
    tabs,
    lastClosed,
    activeId: activeStillOpen ? ws.activeId : id,
    ...withGroupState(tabs, ws.collapsedGroups ?? [], ws.groupColors),
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
 * Puts the tabs in exactly `orderedIds`. Anything that is not a permutation
 * of the open tabs — a stale id, a missing one, a duplicate — is refused and
 * the workspace returned unchanged, so a plan computed against an older strip
 * cannot drop or duplicate a tab.
 */
export function arrangeTabs(ws: WorkspaceTabs, orderedIds: readonly string[]): WorkspaceTabs {
  if (orderedIds.length !== ws.tabs.length) return ws;
  const byId = new Map(ws.tabs.map((tab) => [tab.id, tab]));
  const tabs: TabRecord[] = [];
  const seen = new Set<string>();
  for (const id of orderedIds) {
    const tab = byId.get(id);
    if (!tab || seen.has(id)) return ws;
    seen.add(id);
    tabs.push(tab);
  }
  if (tabs.every((tab, index) => tab === ws.tabs[index])) return ws;
  return { ...ws, tabs };
}

/**
 * Sets or clears one tab's own color. An unknown value is refused and the
 * workspace is returned unchanged. Null clears.
 */
export function setTabColor(ws: WorkspaceTabs, id: string, rawColor: unknown): WorkspaceTabs {
  const tab = ws.tabs.find((item) => item.id === id);
  if (!tab) return ws;
  const color = interpretColorEdit(rawColor);
  if (color === undefined) return ws;
  if ((normalizeTabColor(tab.color) ?? null) === color) return ws;
  return {
    ...ws,
    tabs: ws.tabs.map((item) => (item.id === id ? { ...item, color } : item)),
  };
}

/**
 * Sets or clears the color of a group that currently has members.
 * Renaming the group carries the color. The destination keeps its own color
 * when two groups fold together.
 */
export function setGroupColor(ws: WorkspaceTabs, rawGroup: string, rawColor: unknown): WorkspaceTabs {
  const group = normalizeGroupName(rawGroup);
  if (!group || !liveGroupNames(ws.tabs).has(group)) return ws;
  const color = interpretColorEdit(rawColor);
  if (color === undefined) return ws;
  if (lookupGroupColor(ws.groupColors, group) === color) return ws;
  const without = (ws.groupColors ?? []).filter((entry) => entry.group !== group);
  return {
    ...ws,
    groupColors: color ? [...without, { group, color }] : without,
  };
}

/** Puts restored group colors back once every tab exists, then drops orphans. */
export function restoreGroupColors(
  ws: WorkspaceTabs,
  colors: readonly GroupColor[] | undefined,
): WorkspaceTabs {
  return { ...ws, groupColors: cleanGroupColors(ws.tabs, colors) };
}

export function setTabGroup(ws: WorkspaceTabs, id: string, rawGroup: string | null): WorkspaceTabs {
  const group = normalizeGroupName(rawGroup);
  if (!ws.tabs.some((t) => t.id === id)) return ws;
  const tabs = ws.tabs.map((tab) => (tab.id === id ? { ...tab, group } : tab));
  return {
    ...ws,
    tabs,
    ...withGroupState(tabs, ws.collapsedGroups ?? [], ws.groupColors),
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
    ...withGroupState(
      tabs,
      Array.from(new Set(collapsedGroups)),
      renameGroupColors(ws.groupColors, oldGroup, newGroup),
    ),
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
    ...withGroupState(tabs, ws.collapsedGroups ?? [], ws.groupColors),
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
export function groupByParentFolder(
  ws: WorkspaceTabs,
  repositoryRoot: (tab: TabRecord) => string | null | undefined = () => null,
): WorkspaceTabs {
  const mapped = ws.tabs.map((tab) => {
    // A worktree is grouped by where its repository lives, not by the
    // directory it was checked out into: every agent worktree sits under some
    // `.claude/worktrees/`, and grouping by that put them all in one
    // "worktrees" group, away from the repository they belong to.
    const parent = parentFolderName(repositoryRoot(tab) || tab.path);
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
    ...withGroupState(clustered, ws.collapsedGroups ?? [], ws.groupColors),
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


