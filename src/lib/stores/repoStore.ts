import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
import { diagnostics } from "../diagnostics/diagnostics";
import { harnessStore, type PolicyVerdict } from "./harnessStore";
import type { BranchInfo, TagInfo } from "../branches/types";
import { filterStore, type FilterState } from "./filterStore";
import { graphStore, serverFetchableQuery } from "./graphStore";
import type { InvokeFn } from "./graphStore";
import {
  disambiguateLabels,
  displayName,
  identityKey,
  isCaseInsensitiveFs,
  sameRepo,
  type PathIdentityOptions,
} from "../repos/paths";
import {
  activateNext,
  activatePrev,
  activateTab,
  closeOtherTabs,
  closeTab,
  closeTabsToTheRight,
  emptyWorkspace,
  MAX_OPEN_TABS,
  openTab,
  pinTab as pinWorkspaceTab,
  removeRecent as removeWorkspaceRecent,
  reorderTab,
  type WorkspaceTabs,
} from "../repos/tabModel";
import {
  browserStorage,
  loadPersistedWorkspace,
  savePersistedWorkspace,
  workspaceToPersisted,
  type PersistedWorkspace,
  type StorageLike,
  type ViewTab,
} from "../repos/persist";
import { summarizeBulkOutcome } from "../repos/bulkOps";
import {
  STATUS_POLL_INTERVAL_MS,
  shallowRecordListEqual,
  shouldRunStatusPoll,
  statusesEqual,
} from "../repos/statusPoll";
import { debounce, type Debounced } from "../async/debounce";
import { beginGeneration } from "../async/guard";
import type { FilePatch } from "../diff/patchBuilder";
import type { ReleasePublishResult } from "../ops/model";

/** What a mutating action reports back: whether it ran, and under what verdict. */
export interface MutationOutcome<T = unknown> {
  ok: boolean;
  error?: string;
  policy?: PolicyVerdict;
  output?: T;
}

export type { BranchInfo, TagInfo, ViewTab };
export type { InvokeFn };

/** How the current selection was created; decides what a preference flip may refetch. */
export type SelectionKind = "file" | "commit" | "range";

export interface FileStatus {
  path: string;
  old_path?: string;
  status_code: string;
  is_staged: boolean;
  is_conflicted: boolean;
  additions: number;
  deletions: number;
}

export interface ResolvedRepo {
  path: string;
  name: string;
  is_bare: boolean;
}

/** Wire shape of `cmd_branch_stats`; snake_case like every other command. */
interface BranchStatsUpdate {
  name: string;
  tip_commit_id: string;
  is_remote: boolean;
  remote_name: string | null;
  additions: number;
  deletions: number;
  files_changed: number;
  commits_ahead_of_base: number;
  commits_behind_base: number;
}

interface BranchStatsReport {
  compared_to: string;
  updates: BranchStatsUpdate[];
  computed: number;
  cached: number;
  capped: boolean;
  /** Branches whose churn walk errored this call — missing, not pending. */
  compute_failures: number;
}

export interface OpenRepoTab {
  id: string;
  path: string;
  name: string;
  label: string;
  pinned: boolean;
  isActive: boolean;
  isBare: boolean;
  isDirty: boolean;
  isLoading: boolean;
  error: string | null;
  currentBranch: string | null;
  conflictedCount: number;
}

export interface RepoSession {
  id: string;
  path: string;
  name: string;
  isBare: boolean;
  pinned: boolean;
  branches: BranchInfo[];
  tags: TagInfo[];
  currentBranch: string | null;
  defaultBranch: string | null;
  statuses: FileStatus[];
  selectedCommitId: string | null;
  selectedFilePath: string | null;
  /** Which worktree side `selectedDiff` was fetched from; false for commit diffs. */
  selectedIsStaged: boolean;
  /** Whether `selectedDiff` was fetched with whitespace-only changes ignored. */
  selectedIgnoreWhitespace: boolean;
  /**
   * Internal-only: how the current selection was made. Worktree-file
   * selections can be refetched when the whitespace preference flips; a
   * commit/range selection merely records the preference for the next click.
   */
  selectionKind: SelectionKind;
  selectedDiff: string | null;
  activeTab: ViewTab;
  searchQuery: string;
  selectedBranch: string | null;
  commitDraft: string;
  isAmending: boolean;
  isLoading: boolean;
  error: string | null;
  generation: number;
  /** True once this session's first snapshot has landed and rendered. */
  hasHydrated: boolean;
  /** True while this session's progressive branch-stats fetch is in flight. */
  statsPending: boolean;
  /**
   * True when this path's last branch-stats attempt failed outright, so rows
   * would otherwise show fake zeros; cleared by the next successful drain.
   */
  statsFailed: boolean;
}

export interface RepoState {
  openTabs: OpenRepoTab[];
  activeTabId: string | null;
  recentRepos: string[];
  lastClosed: string[];
  currentPath: string | null;
  branches: BranchInfo[];
  tags: TagInfo[];
  currentBranch: string | null;
  defaultBranch: string | null;
  statuses: FileStatus[];
  selectedCommitId: string | null;
  selectedFilePath: string | null;
  selectedIsStaged: boolean;
  selectedIgnoreWhitespace: boolean;
  selectedDiff: string | null;
  activeTab: ViewTab;
  isLoading: boolean;
  error: string | null;
  commitDraft: string;
  isAmending: boolean;
  isBare: boolean;
  /** Hydration epoch of the active session; bumps on every activation. */
  generation: number;
  /** True while the active session's progressive branch-stats fetch is in flight. */
  statsPending: boolean;
  /** True when the active session's last branch-stats attempt failed. */
  statsFailed: boolean;
}

interface InternalState {
  workspace: WorkspaceTabs;
  sessions: Record<string, RepoSession>;
  workspaceError: string | null;
}

export interface RepoStoreDeps {
  invoke?: InvokeFn;
  storage?: StorageLike | null;
  caseInsensitive?: boolean;
  graph?: {
    showRepo(path: string | null): void;
    loadGraph(
      path: string,
      query?: string,
      revision?: string | null,
    ): Promise<void>;
    evict(path: string): void;
  };
  filter?: {
    subscribe(run: (value: FilterState) => void): () => void;
    setSearch(query: string): void;
    selectBranch(branch: string | null): void;
    clear(): void;
  };
}

/**
 * Session generations never restart — not on close+reopen, not on workspace
 * restore, not on store re-creation. A pre-close in-flight hydrate still
 * carries its old (session id, generation) pair; if a fresh incarnation drew
 * generation 1 again, that stale response would pass the guard and overwrite
 * the new session's data.
 */
let sessionGenerationSource = 0;
function nextSessionGeneration(): number {
  return ++sessionGenerationSource;
}

const MENU_RECENT_CAP = 12;
/** Trailing debounce for localStorage writes and the native-menu rebuild. */
const PERSIST_DEBOUNCE_MS = 300;
/**
 * Upper bound of cmd_branch_stats batches drained per fetch. The backend
 * computes at most 96 unique uncached tips per call, so 64 batches cover
 * ~6100 unique tips; past the bound draining stops silently and churn
 * resumes on the next refresh.
 */
export const STATS_DRAIN_MAX_BATCHES = 64;
/** Publish coalesced stats every N batches (and always on the final drain). */
export const STATS_PUBLISH_EVERY = 8;
/** Trailing window that collapses watcher-event storms into one refresh. */
const WATCHER_REFRESH_DEBOUNCE_MS = 200;
/**
 * Watcher events for a repo are dropped for this long after one of our own
 * mutations succeeds there: they are echoes of the mutation's own `.git`
 * writes, and honoring them means every mutation costs TWO full refreshes.
 * The window exceeds Rust DEBOUNCE_MAX_WAIT=2000ms plus this file's 200ms
 * watcher debounce, so a real echo can never slip past it. Trade-off: an
 * unrelated external change landing inside the window is picked up by the
 * next status poll or later watcher event instead of refreshing immediately —
 * accepted, because it halves per-mutation load.
 */
const WATCHER_ECHO_SUPPRESS_MS = 2500;
/**
 * Mutation kinds that rewrite what the open worktree diff pane shows (the
 * staged/unstaged split moves, or the file disappears). After these, the open
 * selection is refetched so the pane stops displaying pre-mutation content.
 */
const REFETCH_SELECTION_KINDS = new Set([
  "stage",
  "unstage",
  "stage-patch",
  "unstage-patch",
  "discard",
  "commit",
]);

function emptyProjected(): RepoState {
  return {
    openTabs: [],
    activeTabId: null,
    recentRepos: [],
    lastClosed: [],
    currentPath: null,
    branches: [],
    tags: [],
    currentBranch: null,
    defaultBranch: null,
    statuses: [],
    selectedCommitId: null,
    selectedFilePath: null,
    selectedIsStaged: false,
    selectedIgnoreWhitespace: false,
    selectedDiff: null,
    activeTab: "history",
    isLoading: false,
    error: null,
    commitDraft: "",
    isAmending: false,
    isBare: false,
    generation: 0,
    statsPending: false,
    statsFailed: false,
  };
}

function createSession(
  tab: { id: string; path: string; pinned: boolean },
  extras: Partial<RepoSession> = {},
): RepoSession {
  return {
    id: tab.id,
    path: tab.path,
    name: extras.name ?? displayName(tab.path),
    isBare: extras.isBare ?? false,
    pinned: tab.pinned,
    branches: extras.branches ?? [],
    tags: extras.tags ?? [],
    currentBranch: extras.currentBranch ?? null,
    defaultBranch: extras.defaultBranch ?? null,
    statuses: extras.statuses ?? [],
    selectedCommitId: extras.selectedCommitId ?? null,
    selectedFilePath: extras.selectedFilePath ?? null,
    selectedIsStaged: extras.selectedIsStaged ?? false,
    selectedIgnoreWhitespace: extras.selectedIgnoreWhitespace ?? false,
    selectionKind: extras.selectionKind ?? "file",
    selectedDiff: extras.selectedDiff ?? null,
    activeTab: extras.activeTab ?? "history",
    searchQuery: extras.searchQuery ?? "",
    selectedBranch: extras.selectedBranch ?? null,
    commitDraft: extras.commitDraft ?? "",
    isAmending: extras.isAmending ?? false,
    isLoading: extras.isLoading ?? false,
    error: extras.error ?? null,
    generation: extras.generation ?? nextSessionGeneration(),
    hasHydrated: extras.hasHydrated ?? false,
    statsPending: extras.statsPending ?? false,
    statsFailed: extras.statsFailed ?? false,
  };
}

function project(internal: InternalState): RepoState {
  const labels = disambiguateLabels(
    internal.workspace.tabs.map((tab) => tab.path),
  );
  const openTabs: OpenRepoTab[] = internal.workspace.tabs.map((tab) => {
    const session = internal.sessions[tab.id];
    const statuses = session?.statuses ?? [];
    return {
      id: tab.id,
      path: tab.path,
      name: session?.name ?? displayName(tab.path),
      label: labels.get(tab.path) ?? displayName(tab.path),
      pinned: tab.pinned,
      isActive: tab.id === internal.workspace.activeId,
      isBare: session?.isBare ?? false,
      isDirty: statuses.some((file) => !file.is_staged || file.is_conflicted),
      isLoading: session?.isLoading ?? false,
      error: session?.error ?? null,
      currentBranch: session?.currentBranch ?? null,
      conflictedCount: statuses.filter((file) => file.is_conflicted).length,
    };
  });
  const active = internal.workspace.activeId
    ? internal.sessions[internal.workspace.activeId]
    : undefined;
  const base = emptyProjected();
  return {
    ...base,
    openTabs,
    activeTabId: internal.workspace.activeId,
    recentRepos: internal.workspace.recents,
    lastClosed: internal.workspace.lastClosed,
    currentPath: active?.path ?? null,
    branches: active?.branches ?? [],
    tags: active?.tags ?? [],
    currentBranch: active?.currentBranch ?? null,
    defaultBranch: active?.defaultBranch ?? null,
    statuses: active?.statuses ?? [],
    selectedCommitId: active?.selectedCommitId ?? null,
    selectedFilePath: active?.selectedFilePath ?? null,
    selectedIsStaged: active?.selectedIsStaged ?? false,
    selectedIgnoreWhitespace: active?.selectedIgnoreWhitespace ?? false,
    selectedDiff: active?.selectedDiff ?? null,
    activeTab: active?.activeTab ?? "history",
    isLoading: active?.isLoading ?? false,
    error: active?.error ?? internal.workspaceError,
    commitDraft: active?.commitDraft ?? "",
    isAmending: active?.isAmending ?? false,
    isBare: active?.isBare ?? false,
    generation: active?.generation ?? 0,
    statsPending: active?.statsPending ?? false,
    statsFailed: active?.statsFailed ?? false,
  };
}

export function createRepoStore(deps: RepoStoreDeps = {}) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  const storage = deps.storage === undefined ? browserStorage() : deps.storage;
  const options: PathIdentityOptions = {
    caseInsensitive: deps.caseInsensitive ?? isCaseInsensitiveFs(),
  };
  const graph = deps.graph ?? graphStore;
  const filters = deps.filter ?? filterStore;

  let internal: InternalState = {
    workspace: emptyWorkspace(),
    sessions: {},
    workspaceError: null,
  };

  const { subscribe, set } = writable<RepoState>(emptyProjected());
  let openEpoch = 0;
  let syncingFilter = false;
  let shortcutLocked = false;

  // --- work-tree status poll ---------------------------------------------
  // One lazy interval for the whole workspace; ticks are no-ops unless an
  // active session could plausibly have drifted.
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let pollInflight = false;
  /** Monotonic poll ordering; a superseded tick's result is discarded. */
  let pollSequenceSource = 0;
  const pollRuns = new Map<string, number>();
  /** Whether the `pagehide` persist-flush listener is currently attached. */
  let pagehideWired = false;

  // Monotonic token source for diff-selection requests. Session `generation`
  // only moves on tab activation, so it cannot order two rapid selections of
  // the same tab; this does.
  const selectionGeneration = beginGeneration();

  function ensureStatusPoll() {
    if (pollTimer !== null || typeof setInterval === "undefined") return;
    pollTimer = setInterval(
      () => void runStatusPoll(),
      STATUS_POLL_INTERVAL_MS,
    );
    // Quitting inside the persist debounce window would drop the newest
    // search query / view tab; `pagehide` fires on close and navigation.
    // Added and removed symmetrically with the poll so repeated
    // stop/start cycles cannot accumulate duplicate listeners.
    if (typeof document !== "undefined" && !pagehideWired) {
      document.addEventListener("pagehide", flushPersist);
      pagehideWired = true;
    }
  }

  /** Stops the workspace poll; the next activation restarts it lazily. */
  function stopStatusPoll() {
    if (pollTimer === null) return;
    clearInterval(pollTimer);
    pollTimer = null;
    if (pagehideWired && typeof document !== "undefined") {
      document.removeEventListener("pagehide", flushPersist);
      pagehideWired = false;
    }
  }

  async function runStatusPoll() {
    const session = activeSession();
    const hidden = typeof document !== "undefined" && document.hidden;
    if (
      !shouldRunStatusPoll({
        hidden,
        hasSession: Boolean(session),
        isLoading: Boolean(session?.isLoading),
        inflight: pollInflight,
      })
    ) {
      return;
    }
    const path = session!.path;
    const generation = session!.generation;
    const sessionId = session!.id;
    // Ordering tokens: the result must lose to BOTH a newer poll and any
    // hydrate started after this tick — otherwise a slow poll lands after a
    // watcher refresh's snapshot and clobbers fresher statuses for up to a
    // full poll interval.
    pollSequenceSource += 1;
    const run = pollSequenceSource;
    const snapshotRunAtStart = snapshotRuns.get(sessionId);
    pollRuns.set(sessionId, run);
    pollInflight = true;
    try {
      const statuses = await invokeFn<FileStatus[]>("cmd_get_status", {
        repoPath: path,
      });
      if (pollRuns.get(sessionId) !== run) return;
      if (snapshotRuns.get(sessionId) !== snapshotRunAtStart) return;
      // A quiet repo returns byte-identical statuses every 6s; republishing
      // them would re-run every subscriber effect app-wide (visible churn in
      // the diff pane). Skip when the session still holds exactly these
      // statuses — a generation change re-applies via applyToSession below.
      const live = internal.sessions[sessionId];
      if (
        live &&
        live.generation === generation &&
        statusesEqual(statuses, live.statuses)
      ) {
        return;
      }
      applyToSession(sessionId, generation, { statuses });
    } catch {
      /* a repo that vanished reports through the next full refresh */
    } finally {
      if (pollRuns.get(sessionId) === run) pollRuns.delete(sessionId);
      pollInflight = false;
    }
  }
  // -----------------------------------------------------------------------
  let lastPersistedPayload: string | null = null;
  /** Recents payload last handed to the native menu; IPC fires only on change. */
  let lastSentRecentsJson: string | null = null;
  let pendingPersist: { data: PersistedWorkspace; recents: string[] } | null =
    null;
  let persistTimer: ReturnType<typeof setTimeout> | null = null;

  function beginShortcut(): boolean {
    if (shortcutLocked) return false;
    shortcutLocked = true;
    queueMicrotask(() => {
      shortcutLocked = false;
    });
    return true;
  }

  function publish() {
    set(project(internal));
    persist();
  }

  /**
   * Persists workspace state. The write + menu IPC are debounced on a short
   * trailing delay so per-keystroke state (commit draft, search query) cannot
   * storm localStorage and the native menu; `flushPersist` runs the pending
   * side effects immediately whenever a critical mutation lands.
   */
  function persist() {
    const data = workspaceToPersisted(internal.workspace, internal.sessions);
    const recents = internal.workspace.recents.slice(0, MENU_RECENT_CAP);
    const payload = JSON.stringify({ data, recents });
    if (payload === lastPersistedPayload) return;
    lastPersistedPayload = payload;
    pendingPersist = { data, recents };
    if (persistTimer !== null || typeof setTimeout === "undefined") return;
    persistTimer = setTimeout(flushPersist, PERSIST_DEBOUNCE_MS);
  }

  function flushPersist() {
    if (persistTimer !== null) {
      clearTimeout(persistTimer);
      persistTimer = null;
    }
    const pending = pendingPersist;
    if (!pending) return;
    pendingPersist = null;
    if (!savePersistedWorkspace(storage, pending.data)) {
      // The write did not land (quota, private mode). Restore the payload
      // and roll the dedup marker back so an identical future state retries
      // instead of being skipped as "already persisted".
      pendingPersist = pending;
      lastPersistedPayload = null;
      if (typeof setTimeout !== "undefined" && persistTimer === null) {
        persistTimer = setTimeout(flushPersist, PERSIST_DEBOUNCE_MS);
      }
    }
    const recentsJson = JSON.stringify(pending.recents);
    if (recentsJson !== lastSentRecentsJson) {
      lastSentRecentsJson = recentsJson;
      void invokeFn("cmd_set_recent_menu", { paths: pending.recents }).catch(
        () => {},
      );
    }
  }

  function replaceWorkspace(next: WorkspaceTabs) {
    internal = { ...internal, workspace: next };
  }

  function putSession(session: RepoSession) {
    internal = {
      ...internal,
      sessions: { ...internal.sessions, [session.id]: session },
    };
  }

  function activeSession(): RepoSession | undefined {
    const id = internal.workspace.activeId;
    return id ? internal.sessions[id] : undefined;
  }

  /**
   * True when `patch` would leave the session's RENDERED state untouched.
   * IPC snapshots arrive with fresh array identities every cycle; without
   * this gate every watcher refresh republished new root state app-wide,
   * re-running each subscriber effect even though nothing visible changed.
   * Array fields compare element-wise over all own keys, so a deep-equal
   * snapshot is recognized and dropped while any new backend field still
   * forces a (safe-direction) publish.
   */
  function patchIsNoop(
    session: RepoSession,
    patch: Partial<RepoSession>,
  ): boolean {
    for (const key of Object.keys(patch) as (keyof RepoSession)[]) {
      const incoming = patch[key];
      if (incoming === session[key]) continue;
      if (
        Array.isArray(incoming) &&
        Array.isArray(session[key]) &&
        shallowRecordListEqual(
          session[key] as unknown as Record<string, unknown>[],
          incoming as unknown as Record<string, unknown>[],
        )
      ) {
        continue;
      }
      return false;
    }
    return true;
  }

  function applyToSession(
    id: string,
    generation: number,
    patch: Partial<RepoSession>,
  ) {
    const session = internal.sessions[id];
    if (!session || session.generation !== generation) return false;
    // A no-op patch must not publish: subscribers treat every store emission
    // as invalidation. Report "advanced" so callers do not retry the work.
    if (patchIsNoop(session, patch)) return false;
    putSession({ ...session, ...patch });
    publish();
    return true;
  }

  /**
   * Bumps a session into a new activation epoch. A new generation orphans any
   * in-flight branch-stats fetch — its settle path is generation-guarded and
   * will never clear this flag — so pending must reset here.
   */
  function bumped(session: RepoSession): RepoSession {
    return {
      ...session,
      generation: session.generation + 1,
      statsPending: false,
    };
  }

  function syncFilterFromSession(session: RepoSession | undefined) {
    syncingFilter = true;
    try {
      if (!session) {
        filters.clear();
        return;
      }
      filters.setSearch(session.searchQuery);
      filters.selectBranch(session.selectedBranch);
    } finally {
      syncingFilter = false;
    }
  }

  /**
   * Presents the session's repository in the graph pane (cached rows render
   * instantly). It deliberately does NOT call loadGraph: the App-level effect
   * owns fetches keyed on path/revision/query — activation re-renders cached
   * rows without a refetch. Freshness comes from refresh(), which loadGraphs
   * directly for the active session on watcher events and after mutations.
   */
  function revealGraph(session: RepoSession | undefined) {
    if (!session) {
      graph.showRepo(null);
      return;
    }
    graph.showRepo(session.path);
  }

  filters.subscribe((value) => {
    if (syncingFilter) return;
    const session = activeSession();
    if (!session) return;
    if (
      session.searchQuery === value.searchQuery &&
      session.selectedBranch === value.selectedBranch
    ) {
      return;
    }
    putSession({
      ...session,
      searchQuery: value.searchQuery,
      selectedBranch: value.selectedBranch,
    });
    publish();
  });

  async function loadSnapshot(path: string): Promise<{
    branches: BranchInfo[];
    statuses: FileStatus[];
    tags: TagInfo[];
    currentBranch: string | null;
    defaultBranch: string | null;
  }> {
    const [branches, statuses, tags] = await Promise.all([
      invokeFn<BranchInfo[]>("cmd_list_branches", { repoPath: path }),
      invokeFn<FileStatus[]>("cmd_get_status", { repoPath: path }),
      invokeFn<TagInfo[]>("cmd_list_tags", { repoPath: path }).catch(
        () => [] as TagInfo[],
      ),
    ]);
    const currentBranch = branches.find((b) => b.is_current)?.name || null;
    const defaultBranch =
      branches.find((b) => b.is_default)?.name || currentBranch || "main";
    return { branches, statuses, tags, currentBranch, defaultBranch };
  }

  async function watch(path: string) {
    try {
      await invokeFn("cmd_watch_repo", { repoPath: path });
    } catch {
      /* watcher is best-effort */
    }
  }

  async function unwatch(path: string) {
    try {
      await invokeFn("cmd_unwatch_repo", { repoPath: path });
    } catch {
      /* unwatch is best-effort */
    }
  }

  async function resolvePath(path: string): Promise<ResolvedRepo> {
    return invokeFn<ResolvedRepo>("cmd_resolve_repo", { repoPath: path });
  }

  /**
   * Ordering token for snapshot fetches. `activateTab` starts a hydrate at
   * generation N; `refresh()` and watcher events start more at the SAME N
   * (refresh never bumps). Generation alone cannot order those, so the older
   * response could resolve last and overwrite fresher data. Only the
   * latest-started fetch may apply.
   */
  const snapshotRuns = new Map<string, number>();

  /**
   * Folds live churn (branch stats merged after previous drains) into a fresh
   * snapshot so content-identical cycles compare equal. The backend snapshot
   * carries bare branches; without this merge every refresh differed from the
   * enriched live state by `compared_to`/churn fields alone and republished
   * forever. A branch keeps its live object when name+remote+tip all match —
   * the same identity the stats drain merges under — and otherwise takes the
   * snapshot's (tip moved ⇒ stale churn must not survive).
   */
  function withCarriedChurn(
    live: RepoSession,
    snapshot: {
      branches: BranchInfo[];
      statuses: FileStatus[];
      tags: TagInfo[];
      currentBranch: string | null;
      defaultBranch: string | null;
    },
  ) {
    if (live.branches.length === 0) return snapshot;
    const key = (b: Pick<BranchInfo, "name" | "is_remote" | "remote_name">) =>
      `${b.is_remote ? "remote" : "local"}:${b.remote_name ?? ""}:${b.name}`;
    const liveByKey = new Map(live.branches.map((b) => [key(b), b]));
    const branches = snapshot.branches.map((b) => {
      const carried = liveByKey.get(key(b));
      return carried && carried.tip_commit_id === b.tip_commit_id ? carried : b;
    });
    return { ...snapshot, branches };
  }

  async function hydrate(id: string, path: string, generation: number) {
    const run = (snapshotRuns.get(id) ?? 0) + 1;
    snapshotRuns.set(id, run);
    try {
      const raw = await loadSnapshot(path);
      if (snapshotRuns.get(id) !== run) return;
      const live = internal.sessions[id];
      const snapshot = live ? withCarriedChurn(live, raw) : raw;
      applyToSession(id, generation, {
        ...snapshot,
        isLoading: false,
        error: null,
        hasHydrated: true,
      });
      // Stats re-drain even when the snapshot was a no-op: a previously
      // failed drain must retry on the next refresh, and the backend
      // memoizes per-tip so an unchanged repo costs one cheap call.
      if (internal.sessions[id]?.generation === generation) {
        void fetchBranchStats(id, path, generation);
      }
    } catch (err: unknown) {
      if (snapshotRuns.get(id) !== run) return;
      applyToSession(id, generation, {
        isLoading: false,
        error: formatError(err),
      });
    }
  }

  // --- progressive branch churn ------------------------------------------
  // Churn arrives via cmd_branch_stats after the snapshot renders. One logical
  // fetch per session at a time: capped reports re-invoke inside the same
  // in-flight slot until drained, and a refresh racing one lets it finish —
  // the tip guard keeps stale merges out, and the next refresh fetches again.
  const statsInflight = new Set<string>();

  function branchStatsKey(
    name: string,
    isRemote: boolean,
    remoteName?: string | null,
  ): string {
    return `${isRemote ? "remote" : "local"}:${remoteName ?? ""}:${name}`;
  }

  async function fetchBranchStats(
    id: string,
    path: string,
    generation: number,
  ) {
    if (statsInflight.has(id)) return;
    statsInflight.add(id);
    // Raise the churn marker only when some branch actually misses stats
    // (or the last attempt failed): after a completed drain every rendered
    // row carries stats, and flipping the flag on every refresh cycle made
    // those markers blink on quiet repos.
    const liveNow = internal.sessions[id];
    if (
      liveNow &&
      liveNow.generation === generation &&
      (liveNow.statsFailed ||
        liveNow.branches.some((b) => b.compared_to === undefined))
    ) {
      applyToSession(id, generation, { statsPending: true });
    }
    const start = internal.sessions[id];
    if (!start || start.generation !== generation) {
      statsInflight.delete(id);
      return;
    }
    let branches = start.branches;
    let dirty = false;
    // `pending` keeps the in-flight marker lit across intermediate publishes;
    // `failed` lands only on the final settle so a mid-drain hiccup that a
    // later batch recovers from never flashes the failure marker.
    const flush = (pending: boolean, failed: boolean) => {
      // A settle that changes nothing must not publish: the pending/failed
      // flip is what makes sidebar churn markers blink on every refresh.
      const live = internal.sessions[id];
      if (
        !dirty &&
        live &&
        live.generation === generation &&
        live.statsPending === pending &&
        live.statsFailed === failed
      ) {
        return;
      }
      if (dirty) {
        applyToSession(id, generation, {
          branches,
          statsPending: pending,
          statsFailed: failed,
        });
        dirty = false;
        return;
      }
      applyToSession(id, generation, {
        statsPending: pending,
        statsFailed: failed,
      });
    };
    try {
      // Only the LAST batch's failure count matters: uncached failures are
      // retried every round, so an early hiccup a later batch recovered from
      // must not taint the settle.
      let lastBatchHadFailures = false;
      let drainedCleanly = false;
      for (let batch = 0; batch < STATS_DRAIN_MAX_BATCHES; batch += 1) {
        const report = await invokeFn<BranchStatsReport>("cmd_branch_stats", {
          repoPath: path,
        });
        const session = internal.sessions[id];
        if (!session || session.generation !== generation) return;

        lastBatchHadFailures = report.compute_failures > 0;

        const updates = new Map(
          report.updates.map((update) => [
            branchStatsKey(update.name, update.is_remote, update.remote_name),
            update,
          ]),
        );
        let batchTouched = false;
        branches = branches.map((branch) => {
          const update = updates.get(
            branchStatsKey(branch.name, branch.is_remote, branch.remote_name),
          );
          if (!update || update.tip_commit_id !== branch.tip_commit_id)
            return branch;
          batchTouched = true;
          return {
            ...branch,
            additions: update.additions,
            deletions: update.deletions,
            files_changed: update.files_changed,
            commits_ahead_of_base: update.commits_ahead_of_base,
            commits_behind_base: update.commits_behind_base,
            compared_to: report.compared_to,
          };
        });
        if (batchTouched) dirty = true;
        const drained = !report.capped;
        if (dirty && (drained || (batch + 1) % STATS_PUBLISH_EVERY === 0)) {
          flush(report.capped, false);
        }
        if (drained) {
          drainedCleanly = true;
          break;
        }
      }
      // Clean drain retires any earlier failure marker; exhausting the batch
      // bound or losing walks to errors must NOT read as success — BranchList
      // renders its "churn unavailable" marker off statsFailed.
      flush(false, !drainedCleanly || lastBatchHadFailures);
    } catch {
      // Final failure: stop posing zeros as data — BranchList renders its
      // "churn unavailable" marker off statsFailed instead.
      flush(false, true);
    } finally {
      statsInflight.delete(id);
    }
  }
  // ------------------------------------------------------------------------

  // --- watcher-storm coalescing -------------------------------------------
  // One trailing debounce per changed repo path; a later event re-arms the
  // same timer instead of queueing overlapping refreshes.
  const watcherRefreshTimers = new Map<string, Debounced<[]>>();

  /**
   * Per-repo deadline until which watcher events count as echoes of our own
   * just-landed mutation rather than external changes; see
   * WATCHER_ECHO_SUPPRESS_MS.
   */
  const mutationEchoUntil = new Map<string, number>();

  function scheduleWatcherRefresh(key: string, run: () => void) {
    let timer = watcherRefreshTimers.get(key);
    if (!timer) {
      timer = debounce(run, WATCHER_REFRESH_DEBOUNCE_MS);
      watcherRefreshTimers.set(key, timer);
    }
    timer();
  }

  const store = {
    subscribe,
    setError: (error: string | null) => {
      // Every user-facing error funnels through here; mirror it into the
      // diagnostics log so the banner's dismissal never loses it.
      if (error) diagnostics.error("repo", error);
      const session = activeSession();
      if (session) {
        putSession({ ...session, error });
      }
      internal = { ...internal, workspaceError: error };
      publish();
    },
    openRepo: async (
      rawPath: string,
      extras: {
        allowBroken?: boolean;
        activate?: boolean;
        pinned?: boolean;
        restore?: {
          viewTab?: ViewTab;
          searchQuery?: string;
          selectedBranch?: string | null;
        };
      } = {},
    ) => {
      const requestId = ++openEpoch;
      internal = { ...internal, workspaceError: null };
      let resolved: ResolvedRepo | null = null;
      try {
        resolved = await resolvePath(rawPath);
      } catch (err: unknown) {
        if (!extras.allowBroken) {
          internal = { ...internal, workspaceError: formatError(err) };
          publish();
          return false;
        }
      }
      const path = resolved?.path ?? rawPath;
      const activate =
        extras.activate === false ? false : requestId === openEpoch;
      // Canonical identity: once cmd_resolve_repo succeeds, resolved.path is
      // THE identity for this repository — tabs, sessions, graph cache, and
      // watchers all key off it. A tab can still sit under the pre-canonical
      // string when a restore entry was opened while the path was broken;
      // adopting it here keeps one physical repo to one tab instead of a
      // duplicate watcher, a split graph cache, and change events that match
      // nothing. Until resolution succeeds, restore entries stay
      // string-normalized only.
      let workspace = internal.workspace;
      let carriedSession: RepoSession | undefined;
      const rawKey = identityKey(rawPath, options);
      const canonicalKey = identityKey(path, options);
      if (resolved && rawKey && canonicalKey && rawKey !== canonicalKey) {
        const aliases = workspace.tabs.filter(
          (tab) =>
            tab.id !== canonicalKey &&
            identityKey(tab.path, options) === rawKey,
        );
        if (aliases.length > 0) {
          const aliasIds = new Set(aliases.map((tab) => tab.id));
          for (const alias of aliases) {
            carriedSession = internal.sessions[alias.id] ?? carriedSession;
            // Broken aliases never loaded a graph, but showRepo may have been
            // pointed at the alias string on activation.
            graph.evict(alias.path);
          }
          workspace = {
            ...workspace,
            tabs: workspace.tabs.filter((tab) => !aliasIds.has(tab.id)),
            // A removed alias must not remain the active pointer; this open
            // is the natural successor even without an activate request.
            activeId:
              workspace.activeId !== null && aliasIds.has(workspace.activeId)
                ? canonicalKey
                : workspace.activeId,
            recents: workspace.recents.filter(
              (item) => identityKey(item, options) !== rawKey,
            ),
            lastClosed: workspace.lastClosed.filter(
              (item) => identityKey(item, options) !== rawKey,
            ),
          };
          const sessions = { ...internal.sessions };
          for (const id of aliasIds) delete sessions[id];
          internal = { ...internal, sessions };
        }
      }
      const hadCanonical = workspace.tabs.some(
        (tab) => tab.id === canonicalKey,
      );
      const carriedPinned = hadCanonical ? undefined : carriedSession?.pinned;
      const opened = openTab(workspace, path, options, {
        pinned: extras.pinned ?? carriedPinned,
        activate,
      });
      if (!opened.ok) {
        internal = {
          ...internal,
          workspaceError:
            opened.reason === "capacity"
              ? `Too many open repositories (max ${MAX_OPEN_TABS}). Close a tab to open another.`
              : "Invalid repository path",
        };
        publish();
        return false;
      }
      replaceWorkspace({
        ...opened.workspace,
        lastClosed: opened.workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      const existing = internal.sessions[opened.id];
      const session = existing
        ? {
            ...bumped(existing),
            path,
            name: resolved?.name ?? existing.name,
            isBare: resolved?.is_bare ?? existing.isBare,
            pinned:
              opened.workspace.tabs.find((tab) => tab.id === opened.id)
                ?.pinned ?? existing.pinned,
            isLoading: !existing.hasHydrated,
            error: resolved
              ? null
              : String(internal.workspaceError ?? "Repository is unavailable"),
          }
        : createSession(
            {
              id: opened.id,
              path,
              pinned: (extras.pinned ?? carriedPinned) === true,
            },
            {
              name: resolved?.name,
              isBare: resolved?.is_bare,
              // An adopted alias tab hands over its state; an explicit
              // restore payload always wins over what the alias carried.
              activeTab: extras.restore?.viewTab ?? carriedSession?.activeTab,
              searchQuery:
                extras.restore?.searchQuery ?? carriedSession?.searchQuery,
              selectedBranch:
                extras.restore?.selectedBranch ??
                carriedSession?.selectedBranch,
              commitDraft: carriedSession?.commitDraft ?? "",
              isAmending: carriedSession?.isAmending ?? false,
              isLoading: Boolean(resolved),
              error: resolved ? null : "Repository is unavailable",
            },
          );
      putSession(session);
      const shouldPresent =
        activate && internal.workspace.activeId === opened.id;
      if (shouldPresent) {
        syncFilterFromSession(session);
        graph.showRepo(session.path);
      }
      publish();
      if (!resolved) return true;
      await watch(path);
      await hydrate(opened.id, path, session.generation);
      const latest = internal.sessions[opened.id];
      if (
        shouldPresent &&
        latest &&
        internal.workspace.activeId === opened.id
      ) {
        revealGraph(latest);
      }
      ensureStatusPoll();
      flushPersist();
      return true;
    },
    pickAndOpenRepo: async () => {
      try {
        const folder = await invokeFn<string | null>("cmd_pick_folder");
        if (folder) {
          await store.openRepo(folder);
        }
      } catch (err: unknown) {
        internal = { ...internal, workspaceError: formatError(err) };
        publish();
      }
    },
    activateTab: async (id: string, extras: { force?: boolean } = {}) => {
      if (!extras.force && internal.workspace.activeId === id) return;
      if (internal.workspace.activeId !== id) {
        const next = activateTab(internal.workspace, id);
        if (next === internal.workspace) return;
        replaceWorkspace(next);
      }
      const session = internal.sessions[id];
      if (!session) {
        publish();
        return;
      }
      syncFilterFromSession(session);
      const activation = bumped(session);
      // Only a session with nothing rendered yet presents a spinner; a
      // background refresh of rendered content must not strobe it.
      putSession({ ...activation, isLoading: !session.hasHydrated });
      publish();
      revealGraph(activation);
      await hydrate(id, session.path, activation.generation);
      ensureStatusPoll();
      flushPersist();
    },
    closeTab: async (id: string) => {
      const session = internal.sessions[id];
      const result = closeTab(internal.workspace, id);
      if (result.reason === "missing") return;
      replaceWorkspace(result.workspace);
      const { [id]: _removed, ...rest } = internal.sessions;
      internal = { ...internal, sessions: rest };
      stopStatusPoll();
      if (session) {
        graph.evict(session.path);
        await unwatch(session.path);
      }
      const next = activeSession();
      if (next) {
        // Activation-by-close: bump the neighbor so the App-owned graph
        // effect refetches exactly once for it.
        putSession(bumped(next));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    closeActiveTab: async () => {
      if (!beginShortcut()) return;
      const id = internal.workspace.activeId;
      if (id) await store.closeTab(id);
    },
    closeOtherTabs: async (id: string) => {
      const keep = internal.sessions[id];
      if (!keep) return;
      const removed = internal.workspace.tabs.filter((tab) => tab.id !== id);
      replaceWorkspace(closeOtherTabs(internal.workspace, id));
      internal = { ...internal, sessions: { [id]: keep } };
      stopStatusPoll();
      for (const tab of removed) {
        graph.evict(tab.path);
        await unwatch(tab.path);
      }
      if (internal.workspace.activeId === id) {
        putSession(bumped(keep));
      }
      ensureStatusPoll();
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    closeTabsToTheRight: async (id: string) => {
      const index = internal.workspace.tabs.findIndex((tab) => tab.id === id);
      if (index < 0) return;
      const removed = internal.workspace.tabs.slice(index + 1);
      replaceWorkspace(closeTabsToTheRight(internal.workspace, id));
      const remaining = new Set(internal.workspace.tabs.map((tab) => tab.id));
      const sessions: Record<string, RepoSession> = {};
      for (const [key, session] of Object.entries(internal.sessions)) {
        if (remaining.has(key)) sessions[key] = session;
      }
      internal = { ...internal, sessions };
      stopStatusPoll();
      for (const tab of removed) {
        graph.evict(tab.path);
        await unwatch(tab.path);
      }
      const revealed = activeSession();
      // When the active tab was among those removed, tabModel reassigns
      // activeId to the clicked tab — bump it so the App-owned graph effect
      // treats it as a fresh activation.
      if (revealed) {
        putSession(bumped(revealed));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    nextTab: async () => {
      if (!beginShortcut()) return;
      const next = activateNext(internal.workspace);
      if (next.activeId && next.activeId !== internal.workspace.activeId) {
        await store.activateTab(next.activeId);
      }
    },
    prevTab: async () => {
      if (!beginShortcut()) return;
      const next = activatePrev(internal.workspace);
      if (next.activeId && next.activeId !== internal.workspace.activeId) {
        await store.activateTab(next.activeId);
      }
    },
    activateTabAt: async (index: number) => {
      const tab = internal.workspace.tabs[index];
      if (tab) await store.activateTab(tab.id);
    },
    reorderTabs: (fromIndex: number, toIndex: number) => {
      replaceWorkspace(reorderTab(internal.workspace, fromIndex, toIndex));
      publish();
    },
    pinTab: (id: string, pinned: boolean) => {
      replaceWorkspace(pinWorkspaceTab(internal.workspace, id, pinned));
      const session = internal.sessions[id];
      if (session) putSession({ ...session, pinned });
      publish();
    },
    reopenLastClosed: async () => {
      const path = internal.workspace.lastClosed[0];
      if (!path) return;
      await store.openRepo(path, { activate: true });
    },
    removeRecent: (path: string) => {
      replaceWorkspace(
        removeWorkspaceRecent(internal.workspace, path, options),
      );
      publish();
    },
    refresh: async (repoPath?: string) => {
      const session = repoPath
        ? Object.values(internal.sessions).find((item) =>
            sameRepo(item.path, repoPath, options),
          )
        : activeSession();
      if (!session) return;
      const generation = session.generation;
      // Spinner only while nothing is rendered; a rendered error stays until
      // this refresh's own outcome replaces or retires it (hydrate settle).
      applyToSession(session.id, generation, {
        isLoading: !session.hasHydrated,
      });
      await hydrate(session.id, session.path, generation);
      const latest = internal.sessions[session.id];
      if (latest && internal.workspace.activeId === session.id) {
        // The backend filters ANY query server-side, but non-path queries are
        // scheduler keys' no-ops (client-side filtering owns those rows).
        // Forwarding one here launders filtered rows into the cached payload
        // for good: clearing the filter reproduces the last-fired key and
        // nothing refetches.
        void graph.loadGraph(
          latest.path,
          serverFetchableQuery(latest.searchQuery),
          latest.selectedBranch,
        );
      }
    },
    handleRepoChanged: async (changedPath?: string | null) => {
      // Watcher events arrive per file; a checkout or rebase fires many at
      // once. Each changed path collapses onto its own trailing window so a
      // storm becomes one refresh; explicit refresh() calls stay undelayed.
      if (!changedPath) {
        scheduleWatcherRefresh("", () => void store.refresh());
        return;
      }
      const session = Object.values(internal.sessions).find((item) =>
        sameRepo(item.path, changedPath, options),
      );
      if (!session) return;
      // Drop echoes of our own recent writes: the explicit refresh() in
      // runMutating already fetched fresh state, so acting on the echo would
      // refresh the whole session twice per mutation. An unrelated external
      // change inside the window is picked up by the next poll or event.
      const echoUntil = mutationEchoUntil.get(session.path);
      if (echoUntil !== undefined && Date.now() < echoUntil) return;
      scheduleWatcherRefresh(
        session.path,
        () => void store.refresh(session.path),
      );
    },
    restoreWorkspace: async () => {
      stopStatusPoll();
      const persisted = loadPersistedWorkspace(storage, options);
      replaceWorkspace({
        ...emptyWorkspace(),
        recents: persisted.recents,
        lastClosed: persisted.lastClosed,
      });
      internal = { ...internal, sessions: {} };
      // Preserve persisted tab order, but activate the previously-active
      // session the moment ITS hydration lands — not after every remaining
      // tab finishes restoring — so the workspace becomes usable without
      // changing the user's tab arrangement.
      const ordered = [...persisted.tabs];
      let activated = false;
      const isActive = (path: string) =>
        !!persisted.activePath && sameRepo(path, persisted.activePath, options);

      for (const tab of ordered) {
        // Always append (activate: false) so restore cannot shuffle tab
        // order. Present the previously-active session as soon as that
        // iteration finishes — remaining tabs keep hydrating behind it.
        await store.openRepo(tab.path, {
          allowBroken: true,
          activate: false,
          pinned: tab.pinned,
          restore: {
            viewTab: tab.viewTab,
            searchQuery: tab.searchQuery,
            selectedBranch: tab.selectedBranch,
          },
        });
        if (!activated && isActive(tab.path)) {
          activated = true;
          const sessionTab = internal.workspace.tabs.find((item) =>
            sameRepo(item.path, tab.path, options),
          );
          if (sessionTab) {
            await store.activateTab(sessionTab.id, { force: true });
          }
        }
      }
      if (activated) return;
      const desired = persisted.activePath
        ? internal.workspace.tabs.find((tab) =>
            sameRepo(tab.path, persisted.activePath ?? "", options),
          )
        : internal.workspace.tabs[0];
      if (desired) {
        await store.activateTab(desired.id, { force: true });
      } else {
        syncFilterFromSession(undefined);
        graph.showRepo(null);
        publish();
      }
    },
    selectFileDiff: async (filePath: string, isStaged: boolean = false) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      // The whitespace preference lives on the session, not on call sites:
      // every refetch of this diff must carry whatever the user last chose.
      const ignoreWhitespace = session.selectedIgnoreWhitespace;
      const token = selectionGeneration.next();
      try {
        const diff = await invokeFn<string>("cmd_get_file_diff", {
          repoPath: session.path,
          filePath,
          isStaged,
          ignoreWhitespace,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, {
          selectedFilePath: filePath,
          selectedCommitId: null,
          selectedDiff: diff,
          selectedIsStaged: isStaged,
          selectedIgnoreWhitespace: ignoreWhitespace,
          selectionKind: "file",
          activeTab: "diff",
        });
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, { error: formatError(err) });
      }
    },
    selectFilePath: (filePath: string) => {
      const session = activeSession();
      if (!session) return;
      // Records the shared file selection WITHOUT fetching a diff or moving
      // tabs: Coverage's file list and Blame's explorer converge on this one
      // site so the selection survives tab switches, and whichever viewer is
      // open reacts through its own effect. The diff fetch remains
      // selectFileDiff's job.
      applyToSession(session.id, session.generation, {
        selectedFilePath: filePath,
        selectedCommitId: null,
        selectionKind: "file",
      });
    },
    setIgnoreWhitespace: (next: boolean) => {
      const session = activeSession();
      if (!session) return;
      const previous = session.selectedIgnoreWhitespace;
      if (previous === next) return;
      // Read the old session's fields BEFORE applyToSession: putSession
      // replaces the stored object with a fresh immutable copy.
      const {
        selectedFilePath: filePath,
        selectedIsStaged: isStaged,
        selectionKind,
      } = session;
      applyToSession(session.id, session.generation, {
        selectedIgnoreWhitespace: next,
      });
      // Only worktree-file selections can be refetched with -w; commit/range
      // selections just record the preference for the next file click.
      if (selectionKind !== "file" || !filePath) return;
      void store.selectFileDiff(filePath, isStaged);
    },
    selectCommitDiff: async (commitId: string) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      const token = selectionGeneration.next();
      try {
        const diff = await invokeFn<string>("cmd_get_commit_diff", {
          repoPath: session.path,
          commitId,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, {
          selectedCommitId: commitId,
          selectedFilePath: null,
          selectedDiff: diff,
          selectedIsStaged: false,
          selectionKind: "commit",
        });
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, { error: formatError(err) });
      }
    },
    /**
     * Opens one file of a commit while keeping the commit as the selection owner.
     */
    selectCommitFileDiff: async (commitId: string, filePath: string) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      const token = selectionGeneration.next();
      try {
        const fileDiff = await invokeFn<string>("cmd_get_commit_file_diff", {
          repoPath: session.path,
          commitId,
          filePath,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, {
          selectedCommitId: commitId,
          selectedFilePath: filePath,
          selectedDiff: fileDiff,
          selectedIsStaged: false,
          selectionKind: "commit",
          activeTab: "diff",
        });
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, { error: formatError(err) });
      }
    },
    selectRangeDiff: async (from: string, to: string) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      const token = selectionGeneration.next();
      try {
        const diff = await invokeFn<string>("cmd_get_range_diff", {
          repoPath: session.path,
          from,
          to,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, {
          selectedFilePath: `${from}...${to}`,
          selectedCommitId: null,
          selectedDiff: diff,
          selectedIsStaged: false,
          selectionKind: "range",
          activeTab: "diff",
        });
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, { error: formatError(err) });
      }
    },
    stageFile: async (filePath: string) =>
      runMutating("stage", filePath, (path) =>
        invokeFn("cmd_stage_file", { repoPath: path, filePath }),
      ),
    unstageFile: async (filePath: string) =>
      runMutating("unstage", filePath, (path) =>
        invokeFn("cmd_unstage_file", { repoPath: path, filePath }),
      ),
    stageSelectivePatch: async (
      filePatch: FilePatch,
      isStaging: boolean = true,
    ) => {
      if (isStaging) {
        return runMutating("stage-patch", filePatch.new_path, (path) =>
          invokeFn("cmd_stage_selective_patch", { repoPath: path, filePatch }),
        );
      } else {
        return runMutating("unstage-patch", filePatch.new_path, (path) =>
          invokeFn("cmd_unstage_selective_patch", {
            repoPath: path,
            filePatch,
          }),
        );
      }
    },
    stageAll: async () => {
      const session = activeSession();
      if (!session) return { ok: false, error: "No active repository" };
      const unstaged = session.statuses.filter((s) => !s.is_staged);
      // One refresh cycle after the whole batch: per-file refreshes ran N
      // sequential spinner/progress storms for what is one user action.
      const outcomes: MutationOutcome[] = [];
      for (const f of unstaged) {
        outcomes.push(
          await runMutating(
            "stage",
            f.path,
            (path) =>
              invokeFn("cmd_stage_file", { repoPath: path, filePath: f.path }),
            { skipRefresh: true },
          ),
        );
      }
      if (unstaged.length > 0) await store.refresh(session.path);
      return summarizeBulkOutcome(outcomes, "staged");
    },
    unstageAll: async () => {
      const session = activeSession();
      if (!session) return { ok: false, error: "No active repository" };
      const staged = session.statuses.filter((s) => s.is_staged);
      const outcomes: MutationOutcome[] = [];
      for (const f of staged) {
        outcomes.push(
          await runMutating(
            "unstage",
            f.path,
            (path) =>
              invokeFn("cmd_unstage_file", {
                repoPath: path,
                filePath: f.path,
              }),
            { skipRefresh: true },
          ),
        );
      }
      if (staged.length > 0) await store.refresh(session.path);
      return summarizeBulkOutcome(outcomes, "unstaged");
    },
    discardChanges: async (filePath: string) =>
      runMutating("discard", filePath, (path) =>
        invokeFn("cmd_discard_changes", { repoPath: path, filePath }),
      ),
    commit: async (message: string, amend: boolean = false) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_commit", { repoPath: path, message, amend }),
      ),
    /**
     * Stage remaining worktree changes and commit the index as one mutation.
     * Conflicts and empty messages are refused by the backend; the frontend
     * surfaces those as the same MutationOutcome as a gated `commit`.
     */
    quickCommit: async (message: string) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_quick_commit", { repoPath: path, message }),
      ),
    checkoutBranch: async (branchName: string) =>
      runMutating("checkout", branchName, (path) =>
        invokeFn("cmd_checkout_branch", { repoPath: path, branchName }),
      ),
    createBranch: async (branchName: string, startPoint?: string) =>
      runMutating("branch", branchName, (path) =>
        invokeFn("cmd_create_branch", {
          repoPath: path,
          branchName,
          startPoint,
        }),
      ),
    renameBranch: async (oldName: string, newName: string) =>
      runMutating("branch-rename", `${oldName} → ${newName}`, (path) =>
        invokeFn("cmd_rename_branch", { repoPath: path, oldName, newName }),
      ),
    deleteBranch: async (branchName: string, force: boolean = false) =>
      runMutating(
        force ? "branch-delete-force" : "branch-delete",
        branchName,
        (path) =>
          invokeFn("cmd_delete_branch", { repoPath: path, branchName, force }),
      ),
    mergeBranch: async (branchName: string, ffOnly: boolean = false) =>
      runMutating("merge", branchName, (path) =>
        invokeFn("cmd_merge_branch", { repoPath: path, branchName, ffOnly }),
      ),
    fetch: async (remote?: string) =>
      runMutating("fetch", remote ?? "origin", (path) =>
        invokeFn("cmd_fetch", { repoPath: path, remote }),
      ),
    pull: async (remote?: string, branch?: string) =>
      runMutating(
        "pull",
        [remote, branch].filter(Boolean).join(" ") || "upstream",
        (path) => invokeFn("cmd_pull", { repoPath: path, remote, branch }),
      ),
    push: async (remote?: string, branch?: string, force: boolean = false) =>
      runMutating(
        force ? "push-force" : "push",
        [remote, branch].filter(Boolean).join(" ") || "upstream",
        (path) =>
          invokeFn("cmd_push", { repoPath: path, remote, branch, force }),
      ),
    reportIssue: async (title: string, body: string, labels: string[] = []) =>
      runMutating<string>("issue-report", title.slice(0, 80), (path) =>
        invokeFn("cmd_github_create_issue", {
          repoPath: path,
          title,
          body,
          labels,
        }),
      ),
    publishRelease: async (tag: string, message: string) =>
      runMutating<ReleasePublishResult>("release-publish", tag, (path) =>
        invokeFn("cmd_publish_release", { repoPath: path, tag, message }),
      ),
    stashSave: async (message?: string) =>
      runMutating("stash", message ?? "", (path) =>
        invokeFn("cmd_stash_save", { repoPath: path, message }),
      ),
    stashPop: async () =>
      runMutating("unstash", "", (path) =>
        invokeFn("cmd_stash_pop", { repoPath: path }),
      ),
    setActiveTab: (tab: ViewTab) => {
      const session = activeSession();
      if (!session) return;
      applyToSession(session.id, session.generation, { activeTab: tab });
      // The view tab is persisted state: flush so quitting right after a
      // switch does not restore the previous one.
      flushPersist();
    },
    inspectCommitInHistory: (commitId: string) => {
      const session = activeSession();
      if (!session || !commitId) return;
      applyToSession(session.id, session.generation, {
        selectedCommitId: commitId,
        selectedFilePath: null,
        selectedDiff: null,
        selectedIsStaged: false,
        selectionKind: "commit",
        activeTab: "history",
      });
      flushPersist();
    },
    setCommitDraft: (message: string) => {
      const session = activeSession();
      if (!session) return;
      applyToSession(session.id, session.generation, { commitDraft: message });
    },
    setAmending: (isAmending: boolean) => {
      const session = activeSession();
      if (!session) return;
      applyToSession(session.id, session.generation, { isAmending });
    },
  };

  /**
   * Runs one mutating Git action and refreshes the session it belongs to.
   *
   * Gated commands come back as `{ policy, output }`: the harness's verdict
   * travels with the result so the UI can tell an action the gate approved from
   * one that ran with no gate available. The verdict is recorded here, in the
   * single place every mutation passes through, rather than at each call site
   * where one would eventually be forgotten — and the same pass files the
   * action into the agent journal, so an agent-driven session stays
   * reconstructible after the fact.
   */
  async function runMutating<T = unknown>(
    kind: string,
    label: string,
    action: (path: string) => Promise<unknown>,
    opts: { skipRefresh?: boolean } = {},
  ): Promise<MutationOutcome<T>> {
    const session = activeSession();
    if (!session) return { ok: false, error: "No repository is open." };
    const path = session.path;
    const generation = session.generation;
    try {
      const result = await action(path);
      // Arm the echo window BEFORE anything downstream observes the change:
      // the mutation's own `.git` writes are about to bounce back as watcher
      // events (see WATCHER_ECHO_SUPPRESS_MS).
      mutationEchoUntil.set(path, Date.now() + WATCHER_ECHO_SUPPRESS_MS);
      const policy = recordPolicyVerdict(result);
      harnessStore.recordAction({
        kind,
        label,
        ok: true,
        verdict: policy ?? null,
      });
      const still = internal.sessions[session.id];
      if (still && still.generation === generation && !opts.skipRefresh) {
        await store.refresh(path);
        // The refresh updates statuses/branches but nothing refetches the
        // open diff pane, which would otherwise show pre-mutation content
        // (e.g. after partial staging) until the user clicked elsewhere.
        // Commit/range selections have no worktree diff to refetch — and
        // neither does a closed diff pane: refetching one unconditionally
        // would yank the user back to Diff from wherever they navigated.
        if (
          REFETCH_SELECTION_KINDS.has(kind) &&
          still.selectionKind === "file" &&
          still.selectedFilePath &&
          !still.selectedCommitId &&
          still.activeTab === "diff"
        ) {
          void store.selectFileDiff(
            still.selectedFilePath,
            still.selectedIsStaged,
          );
        }
      }
      const output = mutationOutput<T>(result);
      return output === undefined
        ? { ok: true, policy }
        : { ok: true, policy, output };
    } catch (err: unknown) {
      // The error is filed on the session, as it always was, and also returned:
      // a caller that must react to a refusal — the commit box keeping the
      // message it was about to commit — should not have to watch shared state
      // to find out whether its own call went through.
      applyToSession(session.id, generation, { error: formatError(err) });
      harnessStore.recordAction({ kind, label, ok: false });
      return { ok: false, error: formatError(err) };
    }
  }

  return store;
}

/**
 * Files a `Guarded<T>` result's verdict with the harness store, and clears the
 * last verdict for an action that carried none.
 */
function recordPolicyVerdict(result: unknown): PolicyVerdict | undefined {
  if (result && typeof result === "object" && "policy" in result) {
    const policy = (result as { policy: PolicyVerdict }).policy;
    if (policy && typeof policy.status === "string") {
      harnessStore.recordVerdict(policy);
      return policy;
    }
  }
  harnessStore.recordVerdict(null);
  return undefined;
}

function mutationOutput<T>(result: unknown): T | undefined {
  if (result && typeof result === "object" && "output" in result) {
    return (result as { output: T }).output;
  }
  return result === undefined ? undefined : (result as T);
}

export const repoStore = createRepoStore();
