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

/** The workspace schema version this build writes and understands. */
export const WORKSPACE_VERSION = 1 as const;

export type ViewTab =
  | "work"
  | "files"
  | "history"
  | "diff"
  | "conflict"
  | "blame"
  | "coverage"
  | "health"
  | "storage"
  | "stack"
  | "pulse"
  | "terminal"
  | "github"
  | "manvi"
  | "reflog";

export const VIEW_TABS: readonly ViewTab[] = [
  "work",
  "files",
  "history",
  "diff",
  "conflict",
  "blame",
  "coverage",
  "health",
  "storage",
  "stack",
  "pulse",
  "terminal",
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

/**
 * Recovers a persisted view id this build no longer registers.
 *
 * `repo` was folded into Work: remotes, stash and submodules are the same
 * repository, shown as a collapsed section on that screen. Unknown or missing
 * ids also land on Work — that is where a session starts, and falling back to
 * Graph would open a different surface than the one the user had (or never
 * chose).
 */
export function migrateViewTab(value: unknown): ViewTab {
  if (isViewTab(value)) return value;
  return "work";
}

/** A raw, not-yet-validated persisted workspace blob. */
type WorkspaceBlob = Record<string, unknown>;

/**
 * Upgrades a blob written in version N to version N+1. Keyed by the version
 * the blob is written IN; a new schema version ships its step here and old
 * clients keep loading instead of discarding the user's state.
 */
const MIGRATIONS: Record<number, (blob: WorkspaceBlob) => WorkspaceBlob> = {
  1: (v1) => v1,
};

let warnedFutureVersion = false;

/**
 * Walks a raw workspace blob's `version` up to the target schema by applying
 * the registered migrations in order, then validates the result. Returns null
 * when the version is unreadable or from a NEWER GitPulse — the caller then
 * falls back to the legacy recovery keys rather than dropping user state
 * silently. Future-version blobs warn exactly once per session.
 *
 * `migrations` and `targetVersion` are seams for tests (and callers that need
 * to preview an upgrade); production uses the module map up to
 * `WORKSPACE_VERSION`.
 */
export function loadMigrated(
  blob: WorkspaceBlob,
  options: PathIdentityOptions,
  migrations: Record<number, (blob: WorkspaceBlob) => WorkspaceBlob> = MIGRATIONS,
  targetVersion: number = WORKSPACE_VERSION,
): PersistedWorkspace | null {
  const version = typeof blob.version === "number" ? Math.trunc(blob.version) : Number.NaN;
  if (!Number.isInteger(version) || version < 0) return null;
  if (version > targetVersion) {
    // Written by a newer build: migrating down would corrupt it, and wiping
    // it would too — ignore this blob but say why, once, not on every read.
    if (!warnedFutureVersion) {
      warnedFutureVersion = true;
      console.warn(
        `[gitpulse] saved workspace version ${version} is newer than supported ${targetVersion}; falling back to legacy state`,
      );
    }
    return null;
  }
  let current = blob;
  for (let v = version; v < targetVersion; v += 1) {
    const migrate = migrations[v];
    if (!migrate) return null;
    current = migrate(current);
  }
  // The migrated shape is still untrusted wire input; sanitize decides what
  // survives into typed state.
  if (!Array.isArray(current.tabs)) return null;
  return sanitizePersisted(current, options);
}

export function loadPersistedWorkspace(
  storage: StorageLike | null,
  options: PathIdentityOptions,
): PersistedWorkspace {
  const empty: PersistedWorkspace = {
    version: WORKSPACE_VERSION,
    tabs: [],
    activePath: null,
    recents: [],
    lastClosed: [],
  };
  if (!storage) return empty;

  const parsed = readJsonObject(storage.getItem(STORAGE_KEY_WORKSPACE));
  if (parsed) {
    const migrated = loadMigrated(parsed, options);
    if (migrated) return migrated;
  }

  const recents = sanitizePathList(readJsonValue(storage.getItem(STORAGE_KEY_RECENT)), options, MAX_RECENT_REPOS);
  const last = normalizeRepoPath(storage.getItem(STORAGE_KEY_LAST_PATH) ?? "");
  const tabs: PersistedTab[] = last
    ? [{ path: last, pinned: false, viewTab: "work", searchQuery: "", selectedBranch: null }]
    : [];
  return {
    version: WORKSPACE_VERSION,
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
    version: WORKSPACE_VERSION,
    tabs: ws.tabs.map((tab) => {
      const session = sessions[tab.id];
      return {
        path: tab.path,
        pinned: tab.pinned,
        viewTab: migrateViewTab(session?.activeTab),
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
      viewTab: migrateViewTab(record.viewTab),
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
    version: WORKSPACE_VERSION,
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
