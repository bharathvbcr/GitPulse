import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { disambiguateLabels, displayName, type PathIdentityOptions } from "./paths";
import { type TabRecord } from "./tabModel";
import { normalizeTabColor } from "./tabColors";
import { familyFromCommonDir } from "./repoFamily";
import { hasUnstagedChanges } from "../files/fileStatus";
import { WATCH_UNKNOWN } from "./watchState";
import { IDLE_OPERATION } from "./operation";
import type { OpenRepoTab, RepoSession, RepoState, InternalState } from "./storeTypes";

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
    selectedCommitId: null,
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
    terminalOpen: false,
    isLoading: false,
    error: null,
    trustRequired: false,
    isAmending: false,
    isBare: false,
    generation: 0,
    statsPending: false,
    statsFailed: false,
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
}

export function createProjectionCache(): ProjectionCache {
  return { tabs: null, labels: new Map(), rows: new Map() };
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
  const openTabs: OpenRepoTab[] = internal.workspace.tabs.map((tab) => {
    const session = internal.sessions[tab.id];
    const active = tab.id === internal.workspace.activeId;
    const label = labels.get(tab.path) ?? displayName(tab.path);
    const kept = cache.rows.get(tab.id);
    if (kept && kept.tab === tab && kept.session === session && kept.active === active && kept.label === label) {
      return kept.row;
    }
    const row = projectTab(tab, session, active, label, options);
    cache.rows.set(tab.id, { tab, session, active, label, row });
    return row;
  });

  const active = internal.workspace.activeId
    ? internal.sessions[internal.workspace.activeId]
    : undefined;
  return projectRest(internal, openTabs, active);
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

function projectRest(
  internal: InternalState,
  openTabs: OpenRepoTab[],
  active: RepoSession | undefined,
): RepoState {
  const base = emptyProjected();
  return {
    ...base,
    openTabs,
    activeTabId: internal.workspace.activeId,
    recentRepos: internal.workspace.recents,
    lastClosed: internal.workspace.lastClosed,
    collapsedGroups: internal.workspace.collapsedGroups ?? [],
    groupColors: internal.workspace.groupColors ?? [],
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
    selectedDiffTruncated: active?.selectedDiffTruncated ?? false,
    selectedDiffTruncationReason: active?.selectedDiffTruncationReason ?? null,
    selectedDiffPending: active?.selectedDiffPending ?? false,
    selectedDiffError: active?.selectedDiffError ?? null,
    selectionKind: active?.selectionKind ?? "file",
    activeTab: active?.activeTab ?? "work",
    viewSections: active?.viewSections ?? {},
    terminalOpen: active?.terminalOpen ?? false,
    isLoading: active?.isLoading ?? false,
    error: active?.error ?? internal.workspaceError,
    trustRequired: active?.trustRequired === true,
    isAmending: active?.isAmending ?? false,
    isBare: active?.isBare ?? false,
    generation: active?.generation ?? 0,
    statsPending: active?.statsPending ?? false,
    statsFailed: active?.statsFailed ?? false,
    operation: active?.operation ?? IDLE_OPERATION,
    stashEntries: active?.stashEntries ?? [],
    stashFailed: active?.stashFailed ?? false,
    stashTruncated: active?.stashTruncated ?? false,
    tagsTruncated: active?.tagsTruncated ?? false,
    tagsFailed: active?.tagsFailed ?? false,
    watch: active?.watch ?? WATCH_UNKNOWN,
    fetchedAt: active?.fetchedAt ?? null,
  };
}
