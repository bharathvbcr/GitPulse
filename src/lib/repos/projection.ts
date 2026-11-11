import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { disambiguateLabels, displayName, type PathIdentityOptions } from "./paths";
import { type TabRecord } from "./tabModel";
import { normalizeTabColor } from "./tabColors";
import { familyFromCommonDir } from "./repoFamily";
import { hasUnstagedChanges } from "../files/fileStatus";
import { WATCH_UNKNOWN } from "./watchState";
import { IDLE_OPERATION } from "./operation";
import type { FileStatus, OpenRepoTab, RepoSession, RepoState, InternalState } from "./storeTypes";

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

export function emptyProjected(): RepoState {
  return {
    openTabs: [],
    activeTabId: null,
    recentRepos: [],
    lastClosed: [],
    collapsedGroups: [],
    groupColors: [],
    currentPath: null,
    branches: [],
    tags: [],
    currentBranch: null,
    defaultBranch: null,
    statuses: [],
    conflictedCount: 0,
    stagedCount: 0,
    selectedCommitId: null,
    selectedCompareBase: null,
    selectedFilePath: null,
    selectedIsStaged: false,
    selectedIgnoreWhitespace: false,
    selectedDiff: null,
    selectedDiffTruncated: false,
    selectedDiffTruncationReason: null,
    selectedDiffPending: false,
    selectedDiffError: null,
    selectionKind: "file",
    activeTab: "work",
    viewSections: {},
    historyList: "commits",
    terminalOpen: false,
    isLoading: false,
    error: null,
    trustRequired: false,
    isAmending: false,
    isBare: false,
    generation: 0,
    statsPending: false,
    statsFailed: false,
    statusUnavailableSince: null,
    operation: IDLE_OPERATION,
    stashEntries: [],
    stashFailed: false,
    stashTruncated: false,
    tagsTruncated: false,
    tagsFailed: false,
    watch: WATCH_UNKNOWN,
    fetchedAt: null,
  };
}

export function createSession(
  tab: { id: string; path: string; pinned: boolean },
  extras: Partial<RepoSession> = {},
): RepoSession {
  return {
    id: tab.id,
    path: tab.path,
    name: extras.name ?? displayName(tab.path),
    isBare: extras.isBare ?? false,
    commonDir: extras.commonDir ?? null,
    pinned: tab.pinned,
    branches: extras.branches ?? [],
    tags: extras.tags ?? [],
    currentBranch: extras.currentBranch ?? null,
    defaultBranch: extras.defaultBranch ?? null,
    statuses: extras.statuses ?? [],
    selectedCommitId: extras.selectedCommitId ?? null,
    selectedCompareBase: extras.selectedCompareBase ?? null,
    selectedFilePath: extras.selectedFilePath ?? null,
    selectedIsStaged: extras.selectedIsStaged ?? false,
    // Seeded from the Settings > Diff & code preference rather than hardcoded
    // off, so "ignore whitespace by default" survives opening the next
    // repository. Read per session, not once at module load, so changing the
    // preference takes effect on the next repository without a restart.
    selectedIgnoreWhitespace:
      extras.selectedIgnoreWhitespace ?? get(interfaceStore).diffIgnoreWhitespace,
    selectionKind: extras.selectionKind ?? "file",
    selectedDiff: extras.selectedDiff ?? null,
    selectedDiffTruncated: extras.selectedDiffTruncated ?? false,
    selectedDiffTruncationReason: extras.selectedDiffTruncationReason ?? null,
    selectedDiffPending: extras.selectedDiffPending ?? false,
    selectedDiffError: extras.selectedDiffError ?? null,
    activeTab: extras.activeTab ?? "work",
    viewSections: { ...(extras.viewSections ?? {}) },
    historyList: extras.historyList ?? "commits",
    terminalOpen: extras.terminalOpen ?? false,
    searchQuery: extras.searchQuery ?? "",
    selectedBranch: extras.selectedBranch ?? null,
    isAmending: extras.isAmending ?? false,
    isLoading: extras.isLoading ?? false,
    error: extras.error ?? null,
    trustRequired: extras.trustRequired ?? false,
    missing: extras.missing ?? false,
    generation: extras.generation ?? nextSessionGeneration(),
    hasHydrated: extras.hasHydrated ?? false,
    statsPending: extras.statsPending ?? false,
    statsFailed: extras.statsFailed ?? false,
    statusPollFailures: extras.statusPollFailures ?? 0,
    statusUnavailableSince: extras.statusUnavailableSince ?? null,
    operation: extras.operation ?? IDLE_OPERATION,
    stashEntries: extras.stashEntries ?? [],
    stashFailed: extras.stashFailed ?? false,
    stashTruncated: extras.stashTruncated ?? false,
    tagsTruncated: extras.tagsTruncated ?? false,
    tagsFailed: extras.tagsFailed ?? false,
    watch: extras.watch ?? WATCH_UNKNOWN,
    fetchedAt: extras.fetchedAt ?? null,
  };
}

/**
 * What `project` kept from its last run, so publishing costs a reference
 * check per unchanged tab rather than a rebuild of every one. Every publish
 * used to re-derive each tab's labels and family from its paths: harmless at
 * 24 tabs, but restore publishes a few times per tab, which made opening a
 * workspace quadratic in its size (500 tabs took 4.5 s, nearly all of it
 * here). A row is reused only when its tab record, its session, its active
 * flag and its label are the same objects and values as last time — the
 * store replaces a session on every change, so that is exactly "nothing
 * this row reads has changed".
 */
export interface ProjectionCache {
  tabs: readonly TabRecord[] | null;
  labels: Map<string, string>;
  rows: Map<string, { tab: TabRecord; session: RepoSession | undefined; active: boolean; label: string; row: OpenRepoTab }>;
  /** The array `project` returned last, handed back while no row changed or moved. */
  openTabs: OpenRepoTab[] | null;
  /** Counts for the statuses array they were taken from; recounted only for a different array. */
  counts: { statuses: readonly FileStatus[]; conflicted: number; staged: number } | null;
}

export function createProjectionCache(): ProjectionCache {
  return { tabs: null, labels: new Map(), rows: new Map(), openTabs: null, counts: null };
}

function countStatuses(cache: ProjectionCache, statuses: readonly FileStatus[]) {
  if (cache.counts?.statuses !== statuses) {
    let conflicted = 0;
    let staged = 0;
    for (const file of statuses) {
      if (file.is_conflicted) conflicted += 1;
      if (file.is_staged) staged += 1;
    }
    cache.counts = { statuses, conflicted, staged };
  }
  return cache.counts;
}

/** Every `OpenRepoTab` field is a primitive, so equal keys and values is equal. */
function sameRow(a: OpenRepoTab, b: OpenRepoTab): boolean {
  const keys = Object.keys(a) as (keyof OpenRepoTab)[];
  return keys.length === Object.keys(b).length && keys.every((key) => Object.is(a[key], b[key]));
}

export function project(
  internal: InternalState,
  options: PathIdentityOptions,
  cache: ProjectionCache = createProjectionCache(),
): RepoState {
  if (cache.tabs !== internal.workspace.tabs) {
    cache.labels = disambiguateLabels(internal.workspace.tabs.map((tab) => tab.path));
    cache.tabs = internal.workspace.tabs;
    // Drop rows for tabs that are gone, so the cache is bounded by the strip.
    const live = new Set(internal.workspace.tabs.map((tab) => tab.id));
    for (const id of cache.rows.keys()) if (!live.has(id)) cache.rows.delete(id);
  }
  const labels = cache.labels;
  const mapped: OpenRepoTab[] = internal.workspace.tabs.map((tab) => {
    const session = internal.sessions[tab.id];
    const active = tab.id === internal.workspace.activeId;
    const label = labels.get(tab.path) ?? displayName(tab.path);
    const kept = cache.rows.get(tab.id);
    if (kept && kept.tab === tab && kept.session === session && kept.active === active && kept.label === label) {
      return kept.row;
    }
    const built = projectTab(tab, session, active, label, options);
    // The store replaces a session on every change, selection and status
    // included, so a rebuilt row is usually equal to the one it replaces.
    // Keeping the old object is what lets the array below keep its identity.
    const row = kept && sameRow(kept.row, built) ? kept.row : built;
    cache.rows.set(tab.id, { tab, session, active, label, row });
    return row;
  });
  // Positionally identical, not merely "every row came from the cache": a
  // drag reorder reuses every row and moves them, and must publish a new array.
  const previous = cache.openTabs;
  const openTabs =
    previous && previous.length === mapped.length && mapped.every((row, i) => row === previous[i])
      ? (previous as OpenRepoTab[])
      : mapped;
  cache.openTabs = openTabs;

  const active = internal.workspace.activeId
    ? internal.sessions[internal.workspace.activeId]
    : undefined;
  const counts = active ? countStatuses(cache, active.statuses) : null;
  return projectRest(internal, openTabs, active, counts?.conflicted ?? 0, counts?.staged ?? 0);
}

function projectTab(
  tab: TabRecord,
  session: RepoSession | undefined,
  isActive: boolean,
  label: string,
  options: PathIdentityOptions,
): OpenRepoTab {
    const statuses = session?.statuses ?? [];
    const family = familyFromCommonDir(session?.commonDir, options);
    return {
      id: tab.id,
      path: tab.path,
      name: session?.name ?? displayName(tab.path),
      label,
      pinned: tab.pinned,
      group: tab.group ?? null,
      color: normalizeTabColor(tab.color),
      family: family?.key ?? null,
      familyRoot: family?.root ?? null,
      isActive,
      isBare: session?.isBare ?? false,
      isDirty: statuses.some((file) => hasUnstagedChanges(file) || file.is_conflicted),
      isLoading: session?.isLoading ?? false,
      error: session?.error ?? null,
      trustRequired: session?.trustRequired === true,
      missing: session?.missing === true,
      currentBranch: session?.currentBranch ?? null,
      conflictedCount: statuses.filter((file) => file.is_conflicted).length,
      changedCount: new Set(statuses.map((file) => file.path)).size,
      watch: session?.watch.status ?? "unknown",
      countsKnown: session?.hasHydrated === true,
    };
}

/**
 * What a field reads as with no repository open. One frozen array rather than a
 * fresh `[]` per publish: the field stores (`repoStore.statuses` and the rest)
 * notify when the reference changes, so a new empty array each time woke every
 * subscriber on every publish while nothing was open.
 */
const NONE: never[] = [];
Object.freeze(NONE);
const NO_SECTIONS: Record<string, string> = Object.freeze({});

function projectRest(
  internal: InternalState,
  openTabs: OpenRepoTab[],
  active: RepoSession | undefined,
  conflictedCount: number,
  stagedCount: number,
): RepoState {
  const base = emptyProjected();
  return {
    ...base,
    openTabs,
    activeTabId: internal.workspace.activeId,
    recentRepos: internal.workspace.recents,
    lastClosed: internal.workspace.lastClosed,
    collapsedGroups: internal.workspace.collapsedGroups ?? NONE,
    groupColors: internal.workspace.groupColors ?? NONE,
    currentPath: active?.path ?? null,
    branches: active?.branches ?? NONE,
    tags: active?.tags ?? NONE,
    currentBranch: active?.currentBranch ?? null,
    defaultBranch: active?.defaultBranch ?? null,
    statuses: active?.statuses ?? NONE,
    conflictedCount,
    stagedCount,
    selectedCommitId: active?.selectedCommitId ?? null,
    selectedCompareBase: active?.selectedCompareBase ?? null,
    selectedFilePath: active?.selectedFilePath ?? null,
    selectedIsStaged: active?.selectedIsStaged ?? false,
    selectedIgnoreWhitespace: active?.selectedIgnoreWhitespace ?? false,
    selectedDiff: active?.selectedDiff ?? null,
    selectedDiffTruncated: active?.selectedDiffTruncated ?? false,
    selectedDiffTruncationReason: active?.selectedDiffTruncationReason ?? null,
    selectedDiffPending: active?.selectedDiffPending ?? false,
    selectedDiffError: active?.selectedDiffError ?? null,
    selectionKind: active?.selectionKind ?? "file",
    activeTab: active?.activeTab ?? "work",
    viewSections: active?.viewSections ?? NO_SECTIONS,
    historyList: active?.historyList ?? "commits",
    terminalOpen: active?.terminalOpen ?? false,
    isLoading: active?.isLoading ?? false,
    error: active?.error ?? internal.workspaceError,
    trustRequired: active?.trustRequired === true,
    isAmending: active?.isAmending ?? false,
    isBare: active?.isBare ?? false,
    generation: active?.generation ?? 0,
    statsPending: active?.statsPending ?? false,
    statsFailed: active?.statsFailed ?? false,
    statusUnavailableSince: active?.statusUnavailableSince ?? null,
    operation: active?.operation ?? IDLE_OPERATION,
    stashEntries: active?.stashEntries ?? NONE,
    stashFailed: active?.stashFailed ?? false,
    stashTruncated: active?.stashTruncated ?? false,
    tagsTruncated: active?.tagsTruncated ?? false,
    tagsFailed: active?.tagsFailed ?? false,
    watch: active?.watch ?? WATCH_UNKNOWN,
    fetchedAt: active?.fetchedAt ?? null,
  };
}
