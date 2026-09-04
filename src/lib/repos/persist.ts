import { identityKey, normalizeRepoPath, type PathIdentityOptions } from "./paths";
// Runtime import, and safe: viewRegistry takes only `import type` from this
// module, which the compiler erases, so the two do not form a runtime cycle.
import { resolveSection } from "../views/viewRegistry";
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

export type ViewTab = "work" | "code" | "history" | "insights";

export const VIEW_TABS: readonly ViewTab[] = ["work", "code", "history", "insights"];

export interface PersistedTab {
  path: string;
  pinned: boolean;
  viewTab: ViewTab;
  /**
   * The section last open in each sectioned view, keyed by view id.
   *
   * Per view, not one value, because a section is a lens on that view's
   * subject: leaving History on Reflog and coming back through Files should
   * return to Reflog, not reset. Additive on the schema — an older build
   * ignores the field and simply opens each view on its default section.
   */
  viewSections?: Record<string, string>;
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
 * Where a retired view's content now lives.
 *
 * A retirement is not a deletion: the content moved, and a restored session
 * has to land on it rather than being dumped at the start of the app. One
 * that became a dock says so, because reopening the old view id must reopen
 * the dock or the user simply loses the surface they left open.
 */
export interface RetiredView {
  /** The view that shows this content now. */
  readonly tab: ViewTab;
  /**
   * The section within that view, when the content became one lens among
   * several. Without it a restored session lands on the right view showing
   * the wrong pane, which reads as the content having been deleted.
   */
  readonly section?: string;
  /** Set when the content became a dock rather than part of a view. */
  readonly dock?: "terminal";
}

/**
 * Views this build no longer registers, and where their sessions land.
 *
 * `repo` was folded into Work: remotes, stash and submodules are the same
 * repository, shown as a collapsed section on that screen. `terminal` became
 * a dock available beneath every view — the PTY always had to outlive a view
 * switch (App mounted it once and hid it thereafter), so a page you could not
 * leave without closing was the wrong shape for it from the start.
 *
 * Kept as data rather than a chain of `if`s: consolidation retires views in
 * batches, and a map is the thing a test can enumerate.
 */
export const RETIRED_VIEWS: Readonly<Record<string, RetiredView>> = {
  repo: { tab: "work" },
  terminal: { tab: "work", dock: "terminal" },
  // Graph, Diff and Reflog all answer "what happened to this repository".
  // As separate tabs the split forced round trips the Diff view had to grow
  // its own commit picker to avoid; as sections they share the selection.
  diff: { tab: "history", section: "diff" },
  reflog: { tab: "history", section: "reflog" },
  // Pulse, Health, Storage and Coverage are the same shape: an on-demand scan
  // of the repository that must report honestly on its own truncation. Four
  // header entries that are empty until someone runs them became one view
  // with four sections and a single scan-card contract.
  pulse: { tab: "insights", section: "pulse" },
  health: { tab: "insights", section: "health" },
  storage: { tab: "insights", section: "storage" },
  coverage: { tab: "insights", section: "coverage" },
  // Work already joins worktrees, pull requests, runs, verdicts and grants
  // into one row each — the GitHub and MANVI views were rendering halves of
  // the same answer beside it, down to issuing the same `cmd_github_context`
  // call twice. Resolve is not a destination either: it is what a blocked
  // row opens into, and Work already sorts blocked rows first.
  github: { tab: "work", section: "remote" },
  manvi: { tab: "work", section: "policy" },
  stack: { tab: "work", section: "stack" },
  conflict: { tab: "work", section: "resolve" },
  // Files and Blame are two readings of one file. Both keyed off
  // `selectedFilePath`, and Blame had grown its own explorer rail and its own
  // path box so a user would not have to walk back to Files for the file they
  // already had open — the same tell Diff gave. As sections of Code the
  // selection survives the switch, so the second set of pickers is what the
  // merge removes.
  files: { tab: "code", section: "explorer" },
  blame: { tab: "code", section: "blame" },
};

/** The retirement record for a persisted id, or null when it is not retired. */
export function retiredViewFor(value: unknown): RetiredView | null {
  if (typeof value !== "string") return null;
  return Object.hasOwn(RETIRED_VIEWS, value) ? RETIRED_VIEWS[value] : null;
}

/**
 * Recovers a persisted view id this build no longer registers.
 *
 * A retired id lands where its content went. Unknown or missing ids land on
 * Work — that is where a session starts, and falling back to Graph would open
 * a different surface than the one the user had (or never chose).
 */
export function migrateViewTab(value: unknown): ViewTab {
  if (isViewTab(value)) return value;
  return retiredViewFor(value)?.tab ?? "work";
}

/**
 * The per-view section map for a restored tab.
 *
 * Two sources, in order: whatever the blob recorded, then the section a
 * retired `viewTab` implies. The retirement wins, because it describes the
 * pane the user was actually looking at when the session was written —
 * restoring "History" without "on the Reflog" is the retirement reading as a
 * deletion.
 *
 * Every id is narrowed by the registry: a section renamed or dropped since
 * the blob was written falls back to the view's default rather than selecting
 * a pane that no longer exists.
 */
export function sanitizeViewSections(
  raw: unknown,
  persistedViewTab?: unknown,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    for (const [view, section] of Object.entries(raw as Record<string, unknown>)) {
      if (!isViewTab(view) || typeof section !== "string") continue;
      const resolved = resolveSection(view, section);
      if (resolved) out[view] = resolved;
    }
  }
  const retired = retiredViewFor(persistedViewTab);
  if (retired?.section) {
    const resolved = resolveSection(retired.tab, retired.section);
    if (resolved) out[retired.tab] = resolved;
  }
  return out;
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
  sessions: Record<
    string,
    {
      activeTab?: ViewTab;
      viewSections?: Record<string, string>;
      searchQuery?: string;
      selectedBranch?: string | null;
    }
  >,
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
        viewSections: sanitizeViewSections(session?.viewSections),
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
      viewSections: sanitizeViewSections(record.viewSections, record.viewTab),
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
