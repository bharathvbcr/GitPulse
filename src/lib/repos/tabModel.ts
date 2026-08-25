import {
  identityKey,
  normalizeRepoPath,
  type PathIdentityOptions,
} from "./paths";

export const MAX_OPEN_TABS = 24;
export const MAX_RECENT_REPOS = 24;
export const MAX_LAST_CLOSED = 16;

export type CloseTabReason = "missing" | "ok";

export interface TabRecord {
  id: string;
  path: string;
  pinned: boolean;
}

export interface WorkspaceTabs {
  tabs: TabRecord[];
  activeId: string | null;
  recents: string[];
  lastClosed: string[];
}

export type OpenTabResult =
  | { ok: true; workspace: WorkspaceTabs; id: string; created: boolean }
  | { ok: false; reason: "invalid" | "capacity"; workspace: WorkspaceTabs };

export function emptyWorkspace(): WorkspaceTabs {
  return { tabs: [], activeId: null, recents: [], lastClosed: [] };
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
}

export function openTab(
  ws: WorkspaceTabs,
  rawPath: string,
  options: PathIdentityOptions,
  extras: { pinned?: boolean; activate?: boolean } = {},
): OpenTabResult {
  const normalized = normalizeRepoPath(rawPath);
  if (!normalized) {
    return { ok: false, reason: "invalid", workspace: ws };
  }
  const id = identityKey(normalized, options);
  const shouldActivate = extras.activate !== false;
  const existing = ws.tabs.find((tab) => tab.id === id);
  if (existing) {
    const tabs = ws.tabs.map((tab) =>
      tab.id === id
        ? {
            ...tab,
            path: normalized,
            pinned: extras.pinned ?? tab.pinned,
          }
        : tab,
    );
    return {
      ok: true,
      created: false,
      id,
      workspace: rememberRecent(
        { ...ws, tabs, activeId: shouldActivate ? id : ws.activeId },
        normalized,
        options,
      ),
    };
  }
  if (ws.tabs.length >= MAX_OPEN_TABS) {
    return { ok: false, reason: "capacity", workspace: ws };
  }
  const tab: TabRecord = {
    id,
    path: normalized,
    pinned: extras.pinned === true,
  };
  return {
    ok: true,
    created: true,
    id,
    workspace: rememberRecent(
      {
        ...ws,
        tabs: [...ws.tabs, tab],
        activeId: shouldActivate ? id : ws.activeId ?? id,
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
    workspace: { ...ws, tabs, activeId, lastClosed },
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
  return { ...ws, tabs: [keep], activeId: keep.id, lastClosed };
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
  };
}

export function activateTab(ws: WorkspaceTabs, id: string): WorkspaceTabs {
  if (!ws.tabs.some((tab) => tab.id === id)) return ws;
  if (ws.activeId === id) return ws;
  return { ...ws, activeId: id };
}

export function activateAt(ws: WorkspaceTabs, index: number): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const clamped = Math.max(0, Math.min(index, ws.tabs.length - 1));
  return { ...ws, activeId: ws.tabs[clamped].id };
}

export function activateNext(ws: WorkspaceTabs): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const index = Math.max(0, ws.tabs.findIndex((tab) => tab.id === ws.activeId));
  const next = (index + 1) % ws.tabs.length;
  return { ...ws, activeId: ws.tabs[next].id };
}

export function activatePrev(ws: WorkspaceTabs): WorkspaceTabs {
  if (ws.tabs.length === 0) return ws;
  const index = Math.max(0, ws.tabs.findIndex((tab) => tab.id === ws.activeId));
  const prev = (index - 1 + ws.tabs.length) % ws.tabs.length;
  return { ...ws, activeId: ws.tabs[prev].id };
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
