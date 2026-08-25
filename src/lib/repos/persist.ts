import { identityKey, normalizeRepoPath, type PathIdentityOptions } from "./paths";
import {
  MAX_LAST_CLOSED,
  MAX_OPEN_TABS,
  MAX_RECENT_REPOS,
  emptyWorkspace,
  type WorkspaceTabs,
} from "./tabModel";

export const STORAGE_KEY_WORKSPACE = "gitpulse_workspace_v1";
export const STORAGE_KEY_RECENT = "gitpulse_recent_repos";
export const STORAGE_KEY_LAST_PATH = "gitpulse_last_repo";

export type ViewTab =
  | "history"
  | "diff"
  | "conflict"
  | "blame"
  | "coverage"
  | "health"
  | "storage"
  | "stack"
  | "github"
  | "manvi"
  | "reflog";

export const VIEW_TABS: readonly ViewTab[] = [
  "history",
  "diff",
  "conflict",
  "blame",
  "coverage",
  "health",
  "storage",
  "stack",
  "github",
  "manvi",
  "reflog",
];

export interface PersistedTab {
  path: string;
  pinned: boolean;
  viewTab: ViewTab;
  searchQuery: string;
  selectedBranch: string | null;
}

export interface PersistedWorkspace {
  version: 1;
  tabs: PersistedTab[];
  activePath: string | null;
  recents: string[];
  lastClosed: string[];
}

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export function memoryStorage(initial: Record<string, string> = {}): StorageLike {
  const map = { ...initial };
  return {
    getItem: (key) => (Object.prototype.hasOwnProperty.call(map, key) ? map[key] : null),
    setItem: (key, value) => {
      map[key] = value;
    },
    removeItem: (key) => {
      delete map[key];
    },
  };
}

export function browserStorage(): StorageLike | null {
  try {
    if (typeof localStorage === "undefined") return null;
    return localStorage;
  } catch {
    return null;
  }
}

export function isViewTab(value: unknown): value is ViewTab {
  return typeof value === "string" && (VIEW_TABS as readonly string[]).includes(value);
}

export function loadPersistedWorkspace(
  storage: StorageLike | null,
  options: PathIdentityOptions,
): PersistedWorkspace {
  const empty: PersistedWorkspace = {
    version: 1,
    tabs: [],
    activePath: null,
    recents: [],
    lastClosed: [],
  };
  if (!storage) return empty;

  const parsed = readJsonObject(storage.getItem(STORAGE_KEY_WORKSPACE));
  if (parsed && parsed.version === 1 && Array.isArray(parsed.tabs)) {
    return sanitizePersisted(parsed, options);
  }

  const recents = sanitizePathList(readJsonValue(storage.getItem(STORAGE_KEY_RECENT)), options, MAX_RECENT_REPOS);
  const last = normalizeRepoPath(storage.getItem(STORAGE_KEY_LAST_PATH) ?? "");
  const tabs: PersistedTab[] = last
    ? [{ path: last, pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null }]
    : [];
  return {
    version: 1,
    tabs,
    activePath: last,
    recents: last ? [last, ...recents.filter((path) => identityKey(path, options) !== identityKey(last, options))] : recents,
    lastClosed: [],
  };
}

export function savePersistedWorkspace(
  storage: StorageLike | null,
  data: PersistedWorkspace
): boolean {
  if (!storage) return false;
  try {
    // Legacy keys first, workspace blob LAST: the loader prefers the blob, so
    // a quota failure mid-write can leave stale legacy keys but never a
    // half-written newer blob contradicting them.
    storage.setItem(STORAGE_KEY_RECENT, JSON.stringify(data.recents.slice(0, MAX_RECENT_REPOS)));
    if (data.activePath) {
      storage.setItem(STORAGE_KEY_LAST_PATH, data.activePath);
    } else {
      storage.removeItem(STORAGE_KEY_LAST_PATH);
    }
    storage.setItem(STORAGE_KEY_WORKSPACE, JSON.stringify(data));
    return true;
  } catch {
    /* quota / private mode — fail closed, keep in-memory state. The false
       return lets callers retry instead of believing the write landed. */
    return false;
  }
}

export function workspaceToPersisted(
  ws: WorkspaceTabs,
  sessions: Record<string, { activeTab?: ViewTab; searchQuery?: string; selectedBranch?: string | null }>,
): PersistedWorkspace {
  const active = ws.tabs.find((tab) => tab.id === ws.activeId);
  return {
    version: 1,
    tabs: ws.tabs.map((tab) => {
      const session = sessions[tab.id];
      return {
        path: tab.path,
        pinned: tab.pinned,
        viewTab: isViewTab(session?.activeTab) ? session.activeTab : "history",
        searchQuery: session?.searchQuery ?? "",
        selectedBranch: session?.selectedBranch ?? null,
      };
    }),
    activePath: active?.path ?? null,
    recents: ws.recents.slice(0, MAX_RECENT_REPOS),
    lastClosed: ws.lastClosed.slice(0, MAX_LAST_CLOSED),
  };
}

function sanitizePersisted(raw: Record<string, unknown>, options: PathIdentityOptions): PersistedWorkspace {
  const tabs: PersistedTab[] = [];
  const seen = new Set<string>();
  const incoming = Array.isArray(raw.tabs) ? raw.tabs : [];
  for (const item of incoming) {
    if (!item || typeof item !== "object") continue;
    const record = item as Record<string, unknown>;
    const path = normalizeRepoPath(typeof record.path === "string" ? record.path : "");
    if (!path) continue;
    const id = identityKey(path, options);
    if (seen.has(id)) continue;
    seen.add(id);
    tabs.push({
      path,
      pinned: record.pinned === true,
      viewTab: isViewTab(record.viewTab) ? record.viewTab : "history",
      searchQuery: typeof record.searchQuery === "string" ? record.searchQuery : "",
      selectedBranch: typeof record.selectedBranch === "string" ? record.selectedBranch : null,
    });
    if (tabs.length >= MAX_OPEN_TABS) break;
  }
  const recents = sanitizePathList(raw.recents, options, MAX_RECENT_REPOS);
  const lastClosed = sanitizePathList(raw.lastClosed, options, MAX_LAST_CLOSED);
  const activePath = normalizeRepoPath(typeof raw.activePath === "string" ? raw.activePath : "");
  const activeExists = activePath && tabs.some((tab) => identityKey(tab.path, options) === identityKey(activePath, options));
  return {
    version: 1,
    tabs,
    activePath: activeExists ? activePath : tabs[0]?.path ?? null,
    recents,
    lastClosed,
  };
}

function sanitizePathList(raw: unknown, options: PathIdentityOptions, cap: number): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  const seen = new Set<string>();
  for (const item of raw) {
    if (typeof item !== "string") continue;
    const path = normalizeRepoPath(item);
    if (!path) continue;
    const id = identityKey(path, options);
    if (seen.has(id)) continue;
    seen.add(id);
    out.push(path);
    if (out.length >= cap) break;
  }
  return out;
}

function readJsonValue(raw: string | null): unknown {
  if (!raw) return null;
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
}

function readJsonObject(raw: string | null): Record<string, unknown> | null {
  const parsed = readJsonValue(raw);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  return parsed as Record<string, unknown>;
}

export { emptyWorkspace };
