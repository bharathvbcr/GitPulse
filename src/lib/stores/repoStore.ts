import type { RebaseStep } from "../rebase/planner";
import { get, writable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { formatError } from "../ui/formatError";
import { diagnostics } from "../diagnostics/diagnostics";
import { askConfirm } from "./modalStore";
// One-way: the terminal registry knows nothing about repositories, so this
// cannot form a cycle. `sessionFocus` takes the store by injection for the
// same reason.
import { sessionsByRepo, terminalSessions } from "../terminal/sessionRegistry";
import { requestRepositoryTrust } from "../repos/repositoryTrust";
import { harnessStore, type PolicyVerdict } from "./harnessStore";
import { parseTagList, type BranchInfo, type TagInfo } from "../branches/types";
import { filterStore, type FilterState } from "./filterStore";
import { graphStore } from "./graphStore";
import { interfaceStore } from "./interfaceStore";
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
  activateTab,
  closeOtherTabs,
  closeTab,
  closeTabsToTheRight,
  emptyWorkspace,
  MAX_OPEN_TABS,
  openTab,
  pinTab as pinWorkspaceTab,
  removeRecent as removeWorkspaceRecent,
  moveTabTo as moveWorkspaceTabTo,
  arrangeTabs as arrangeWorkspaceTabs,
  setTabGroup as setWorkspaceTabGroup,
  setTabColor as setWorkspaceTabColor,
  setGroupColor as setWorkspaceGroupColor,
  restoreGroupColors,
  setGroupCollapsed as setWorkspaceGroupCollapsed,
  toggleGroupCollapsed as toggleWorkspaceGroupCollapsed,
  isGroupCollapsed as isWorkspaceGroupCollapsed,
  renameGroup as renameWorkspaceGroup,
  ungroupTabs as ungroupWorkspaceTabs,
  closeGroup as closeWorkspaceGroup,
  groupByParentFolder as groupWorkspaceByParentFolder,
  collapseAllGroups as collapseWorkspaceAllGroups,
  expandAllGroups as expandWorkspaceAllGroups,
  type WorkspaceTabs,
} from "../repos/tabModel";
import {
  browserStorage,
  loadPersistedWorkspace,
  savePersistedWorkspace,
  workspaceToPersisted,
  type StorageLike,
  type ViewTab,
} from "../repos/persist";
import * as workspaceSync from "../codeintel/workspaceSync";
import { autoInit } from "../codeintel/autoInit";
import { liveIndex } from "../codeintel/liveIndex";
import { normalizeTabColor, type GroupColor, type TabColor } from "../repos/tabColors";
import { familyFromCommonDir } from "../repos/repoFamily";
import { computeTabLayout, type StackingOptions } from "../repos/tabGroups";
import {
  activationStops,
  canMoveUnit,
  cycleStop,
  moveUnit,
  moveUnitToEdge,
  unitForTab,
} from "../repos/stripNav";
import { expandedStacks, lastUsedCheckouts, noteActiveCheckout } from "../repos/stackState";
import { isSectionOnScreen, resolveSection } from "../views/viewRegistry";
import { parseStashList, type StashAction, type StashEntry, type StashSaveOptions } from "../repos/stash";
import { hasUnstagedChanges } from "../files/fileStatus";
import { indexSelectionPaths } from "../repos/bulkOps";
import {
  WATCH_ACTIVE,
  WATCH_UNKNOWN,
  needsFullPoll,
  watchFailed,
  watchStatesEqual,
  type WatchState,
} from "../repos/watchState";
import { parseRemoteList, type RemoteChange, type RemoteInfo } from "../repos/remotes";
import { parseSubmoduleList, type SubmoduleChange, type SubmoduleInfo } from "../repos/submodules";
import {
  runAcrossRepos,
  type BulkRunReport,
  type RepoTarget,
  type RunOptions,
} from "../repos/workspaceOps";
import { mapItems, DEFAULT_FAN_OUT } from "../async/pool";
import {
  summarizeWorkspace,
  bulkSkipReason,
  type RepoWipInput,
  type WorkspaceWip,
} from "../repos/wipSummary";
import { toWipInput, type RepoFacts } from "../repos/facts";
import {
  IDLE_OPERATION,
  operationStatesEqual,
  type OperationAction,
  type OperationState,
  type RepoOperation,
} from "../repos/operation";
import {
  STATUS_POLL_INTERVAL_MS,
  shallowRecordListEqual,
  shouldRunStatusPoll,
  statusesEqual,
} from "../repos/statusPoll";
import {
  bindForegroundChanges,
  readBackgroundDocument,
  readHiddenDocument,
} from "../runtime/foreground";
import {
  WATCHER_REFRESH_DEBOUNCE_MS,
  createWatcherRefreshPolicy,
} from "../repos/watcherRefresh";
import { decideCadence, readEventLoopDelay } from "../runtime/loadCadence";
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
export type { OperationAction, OperationState, RepoOperation };
export type { StashAction, StashEntry, RemoteChange, SubmoduleChange };
export type { WatchState };
export type { BulkRunReport, WorkspaceWip };
export type { RepoFacts };
export type { InvokeFn };

/** Mirrors the Rust `ResetMode` under `rename_all = "lowercase"`. */
export type ResetMode = "soft" | "mixed" | "keep" | "hard";
export type IndexAction = "stage" | "unstage";

/** How the current selection was created; decides what a preference flip may refetch. */
export type SelectionKind = "file" | "commit" | "range";

export interface FileStatus {
  path: string;
  old_path?: string | null;
  status_code: string;
  is_staged: boolean;
  is_conflicted: boolean;
  additions: number;
  deletions: number;
  staged_additions?: number;
  staged_deletions?: number;
  unstaged_additions?: number;
  unstaged_deletions?: number;
  /**
   * Why this row's additions/deletions may understate reality — its numstat
   * record could not be parsed. Rust omits the key entirely while empty, so
   * this is absent on the overwhelming majority of rows.
   */
  warnings?: string[];
}

export interface ResolvedRepo {
  path: string;
  name: string;
  is_bare: boolean;
  /**
   * Canonical common Git directory, shared by every worktree of the
   * repository. Optional on the wire: older fixtures and a backend that could
   * not read the metadata both leave the tab without a family.
   */
  common_dir?: string | null;
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

export interface BranchStatsReport {
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
  group?: string | null;
  /** Own color. Missing and null both inherit the group color, when the group has one. */
  color?: TabColor | null;
  /**
   * Repository family: one key shared by every checkout of a repository
   * (see repos/repoFamily.ts). Null until resolved, or when unreadable.
   */
  family?: string | null;
  /** The family's own directory, for naming it; null with `family`. */
  familyRoot?: string | null;
  isActive: boolean;
  isBare: boolean;
  isDirty: boolean;
  isLoading: boolean;
  error: string | null;
  /**
   * Set when opening or hydrating this tab failed because the repository
   * is not trusted. Cleared only by a hydrate that succeeds. Background
   * work skips these paths so a refusal stays one state, not a stream.
   */
  trustRequired?: boolean;
  currentBranch: string | null;
  conflictedCount: number;
  changedCount: number;
}

/**
 * Wire shape of every diff-returning command.
 *
 * `truncated` is not optional: a payload that forgot it would default to
 * "complete", which is the exact failure the flag exists to prevent.
 */
export interface DiffPayload {
  text: string;
  truncated: boolean;
  /**
   * Why the text is a prefix, as a clause to render after naming the subject;
   * null when the diff is whole. The banner used to assert "larger than we
   * read in one go" for every cut, which is one of two possible causes.
   */
  truncation_reason: string | null;
}

export interface RepoSession {
  id: string;
  path: string;
  name: string;
  isBare: boolean;
  /** From `ResolvedRepo.common_dir`; null until resolved or when unreadable. */
  commonDir: string | null;
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
  /**
   * True when the backend cut this diff at its read budget.
   *
   * A prefix rendered as a whole diff is a lie the viewer cannot detect on
   * its own: the last hunk on screen looks like the last hunk in the commit.
   * The flag drives both the notice and the staging lockout, because staging
   * a hunk from a prefix stages less than the rows imply.
   */
  selectedDiffTruncated: boolean;
  /** Why it is a prefix, when it is; null when whole. See `DiffPayload`. */
  selectedDiffTruncationReason: string | null;
  /**
   * True while a newly selected diff is still being fetched.
   *
   * The store publishes a selection's fields atomically on completion, so
   * without this the pane keeps showing the PREVIOUS file — its path, its
   * line count, its rows — for as long as the read takes. On a large commit
   * that is over a second of a viewer confidently displaying the wrong file.
   *
   * Only a change of target raises it. A refetch of the diff already on
   * screen (the one that follows a stage, or a watcher-driven refresh) leaves
   * it false, because flashing a skeleton over content that is about to be
   * replaced by nearly the same content is worse than the staleness it
   * announces.
   */
  selectedDiffPending: boolean;
  activeTab: ViewTab;
  /**
   * The section last open in each sectioned view, keyed by view id.
   *
   * Per view rather than one value: a section is a lens on that view's
   * subject, so leaving History on Reflog and returning through Files should
   * come back to Reflog. Unset views open on their registered default.
   */
  viewSections: Record<string, string>;
  /**
   * Whether the terminal dock is showing on THIS repository tab.
   *
   * Per tab because a shell belongs to a working tree. As one workspace-wide
   * flag, opening a terminal in one repository opened the dock over every
   * other repository the user switched to — and since hosting a panel starts
   * a shell, it also spawned a process in each, spending the global
   * `MAX_PTY_SESSIONS` budget on repositories nobody asked for a shell in.
   *
   * Closing the dock only hides it; the shells keep running and their
   * scrollback survives. Closing the repository TAB is what ends them.
   */
  terminalOpen: boolean;
  searchQuery: string;
  selectedBranch: string | null;
  commitDraft: string;
  isAmending: boolean;
  isLoading: boolean;
  error: string | null;
  /**
   * True when the last open or hydrate of this session failed with
   * `REPOSITORY_TRUST_REQUIRED`. A later successful hydrate clears it.
   * Schedulers treat it as "do not call the backend for this path".
   */
  trustRequired: boolean;
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
  /**
   * The multi-step git operation this worktree is parked in, if any, plus
   * whether the probe itself failed. Refreshed with every snapshot: an
   * operation can start or end from the terminal panel, another GitPulse
   * window, or an agent, so it is never inferred from our own mutations.
   */
  operation: OperationState;
  /**
   * The stash stack. Carried on the session because it is part of the
   * work-in-progress answer: a stash is work that exists only here, and it is
   * invisible from every other surface in the app.
   */
  stashEntries: StashEntry[];
  /** True when the stash probe failed, so an empty list is not read as "none". */
  stashFailed: boolean;
  stashTruncated: boolean;
  /** True when older tags exist beyond the listing cap. */
  tagsTruncated: boolean;
  /** True when the tag list could not be read, so empty is not "no tags". */
  tagsFailed: boolean;
  /**
   * Whether this repository is receiving live filesystem updates.
   *
   * A failed watch used to be swallowed, leaving the session indistinguishable
   * from a watched one while its branches, graph and operation state went
   * stale. Recorded here so the poll can compensate and the UI can say so.
   */
  watch: WatchState;
  /** Epoch ms of FETCH_HEAD mtime; null when absent or unread. */
  fetchedAt: number | null;
}

export interface RepoState {
  openTabs: OpenRepoTab[];
  activeTabId: string | null;
  recentRepos: string[];
  lastClosed: string[];
  collapsedGroups: string[];
  groupColors: GroupColor[];
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
  /**
   * True when the backend cut this diff at its read budget.
   *
   * A prefix rendered as a whole diff is a lie the viewer cannot detect on
   * its own: the last hunk on screen looks like the last hunk in the commit.
   * The flag drives both the notice and the staging lockout, because staging
   * a hunk from a prefix stages less than the rows imply.
   */
  selectedDiffTruncated: boolean;
  /** Why it is a prefix, when it is; null when whole. See `DiffPayload`. */
  selectedDiffTruncationReason: string | null;
  /**
   * True while a newly selected diff is still being fetched.
   *
   * The store publishes a selection's fields atomically on completion, so
   * without this the pane keeps showing the PREVIOUS file — its path, its
   * line count, its rows — for as long as the read takes. On a large commit
   * that is over a second of a viewer confidently displaying the wrong file.
   *
   * Only a change of target raises it. A refetch of the diff already on
   * screen (the one that follows a stage, or a watcher-driven refresh) leaves
   * it false, because flashing a skeleton over content that is about to be
   * replaced by nearly the same content is worse than the staleness it
   * announces.
   */
  selectedDiffPending: boolean;
  activeTab: ViewTab;
  /** Section last open in each sectioned view; see the session field. */
  viewSections: Record<string, string>;
  /** Whether the ACTIVE tab is showing the terminal dock; see the session field. */
  terminalOpen: boolean;
  isLoading: boolean;
  error: string | null;
  /** Active session needs trust before Git or background work may run. */
  trustRequired: boolean;
  commitDraft: string;
  isAmending: boolean;
  isBare: boolean;
  /** Hydration epoch of the active session; bumps on every activation. */
  generation: number;
  /** True while the active session's progressive branch-stats fetch is in flight. */
  statsPending: boolean;
  /** True when the active session's last branch-stats attempt failed. */
  statsFailed: boolean;
  /** The active session's parked operation, if any. */
  operation: OperationState;
  /** The active session's stash stack. */
  stashEntries: StashEntry[];
  /** True when the active session's stash probe failed. */
  stashFailed: boolean;
  stashTruncated: boolean;
  /** True when older tags exist beyond the listing cap. */
  tagsTruncated: boolean;
  /** True when the active session's tag list could not be read. */
  tagsFailed: boolean;
  /** Whether the active session is receiving live filesystem updates. */
  watch: WatchState;
  /** Epoch ms of FETCH_HEAD mtime for the active repo; null when never fetched. */
  fetchedAt: number | null;
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
  /**
   * How many live terminal sessions a repository holds.
   *
   * Injected rather than imported so the store stays testable without the
   * terminal module, matching `graph` and `filter`. Closing a repository tab
   * unmounts its terminal panel, which kills every shell in it — including a
   * build or an agent still running — and that used to happen silently. It
   * became likely enough to guard once the dock went per repository: before,
   * only the repository in front could be holding one.
   */
  terminals?: {
    countFor(repoPath: string): number;
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
/**
 * Watcher events for a repo are dropped for this long after one of our own
 * mutations succeeds there: they are echoes of the mutation's own `.git`
 * writes, and honoring them means every mutation costs TWO full refreshes.
 * The window exceeds Rust DEBOUNCE_MAX_WAIT=2000ms plus the 200ms watcher
 * debounce (WATCHER_REFRESH_DEBOUNCE_MS), so a real echo can never slip past
 * it. The check runs when the event arrives, before the refresh policy
 * delays anything, so a background tab's longer wait does not widen it.
 * Trade-off: an
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
  "stage-all",
  "unstage-all",
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
    activeTab: "work",
    viewSections: {},
    terminalOpen: false,
    isLoading: false,
    error: null,
    trustRequired: false,
    commitDraft: "",
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

function createSession(
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
    activeTab: extras.activeTab ?? "work",
    viewSections: { ...(extras.viewSections ?? {}) },
    terminalOpen: extras.terminalOpen ?? false,
    searchQuery: extras.searchQuery ?? "",
    selectedBranch: extras.selectedBranch ?? null,
    commitDraft: extras.commitDraft ?? "",
    isAmending: extras.isAmending ?? false,
    isLoading: extras.isLoading ?? false,
    error: extras.error ?? null,
    trustRequired: extras.trustRequired ?? false,
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

function project(internal: InternalState, options: PathIdentityOptions): RepoState {
  const labels = disambiguateLabels(
    internal.workspace.tabs.map((tab) => tab.path),
  );
  const openTabs: OpenRepoTab[] = internal.workspace.tabs.map((tab) => {
    const session = internal.sessions[tab.id];
    const statuses = session?.statuses ?? [];
    const family = familyFromCommonDir(session?.commonDir, options);
    return {
      id: tab.id,
      path: tab.path,
      name: session?.name ?? displayName(tab.path),
      label: labels.get(tab.path) ?? displayName(tab.path),
      pinned: tab.pinned,
      group: tab.group ?? null,
      color: normalizeTabColor(tab.color),
      family: family?.key ?? null,
      familyRoot: family?.root ?? null,
      isActive: tab.id === internal.workspace.activeId,
      isBare: session?.isBare ?? false,
      isDirty: statuses.some((file) => hasUnstagedChanges(file) || file.is_conflicted),
      isLoading: session?.isLoading ?? false,
      error: session?.error ?? null,
      trustRequired: session?.trustRequired === true,
      currentBranch: session?.currentBranch ?? null,
      conflictedCount: statuses.filter((file) => file.is_conflicted).length,
      changedCount: new Set(statuses.map((file) => file.path)).size,
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
    activeTab: active?.activeTab ?? "work",
    viewSections: active?.viewSections ?? {},
    terminalOpen: active?.terminalOpen ?? false,
    isLoading: active?.isLoading ?? false,
    error: active?.error ?? internal.workspaceError,
    trustRequired: active?.trustRequired === true,
    commitDraft: active?.commitDraft ?? "",
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

const REPOSITORY_TRUST_REQUIRED = "REPOSITORY_TRUST_REQUIRED";

export function repositoryTrustRefused(message: string): boolean {
  return message.includes(REPOSITORY_TRUST_REQUIRED);
}

/**
 * Open paths that may receive background work. A session with
 * `trustRequired` is omitted, and it is not used as the active key.
 */
export function pathsTrustedForBackground(
  tabs: readonly { path: string; trustRequired?: boolean }[],
  activePath: string | null,
): { activeKey: string | null; retainedKeys: string[] } {
  const retainedKeys: string[] = [];
  let activeBlocked = false;
  for (const tab of tabs) {
    if (tab.trustRequired) {
      if (tab.path === activePath) activeBlocked = true;
      continue;
    }
    retainedKeys.push(tab.path);
  }
  return {
    activeKey: activePath && !activeBlocked ? activePath : null,
    retainedKeys,
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
  const terminals = deps.terminals ?? {
    countFor: (repoPath: string) => sessionsByRepo(get(terminalSessions)).get(repoPath) ?? 0,
  };

  /**
   * True when these tabs may be closed: either nothing is running in any of
   * them, or the user said to end it anyway.
   *
   * Takes a LIST because "Close Other Tabs" and "Close Tabs to the Right"
   * discard several repositories at once and never route through `closeTab`.
   * Guarding only the single close would leave the two paths that can lose
   * the most work unguarded.
   *
   * Fails OPEN on a broken counter. A registry that threw would otherwise
   * make every repository tab unclosable, which is a worse failure than a
   * missing warning — the count is a courtesy, not a safety interlock.
   */
  async function confirmTerminalLoss(paths: readonly string[]): Promise<boolean> {
    let running = 0;
    let repos = 0;
    try {
      for (const path of paths) {
        const count = terminals.countFor(path);
        if (!Number.isFinite(count) || count <= 0) continue;
        running += count;
        repos += 1;
      }
    } catch {
      return true;
    }
    if (running <= 0) return true;
    const shells = running === 1 ? "the shell" : `all ${running} shells`;
    const subject = repos === 1 ? "this repository tab" : `these ${repos} repository tabs`;
    return askConfirm({
      title: running === 1 ? "End the terminal session?" : `End ${running} terminal sessions?`,
      message:
        `${paths.length === 1 ? `${paths[0]}\n\n` : ""}` +
        `Closing ${subject} ends ${shells} running in ${repos === 1 ? "it" : "them"}. ` +
        "A command still running — a build, a test run, an agent — is stopped.\n\n" +
        "Hiding the terminal instead (⌃`) leaves it running.",
      confirmLabel: running === 1 ? "Close and End Session" : "Close and End Sessions",
      destructive: true,
    });
  }

  let internal: InternalState = {
    workspace: emptyWorkspace(),
    sessions: {},
    workspaceError: null,
  };

  const { subscribe, set } = writable<RepoState>(emptyProjected());
  // A full watcher/manual refresh can observe M→M content edits while every
  // status field stays equal. Keep this signal separate from the UI snapshot
  // so content readers update without invalidating every repository subscriber.
  const contentRevisions = writable<Record<string, string>>({});
  let openEpoch = 0;
  let syncingFilter = false;
  let shortcutLocked = false;

  // --- work-tree status poll ---------------------------------------------
  // One lazy timer for the whole workspace. While the window is in the
  // background the timer is gone, not merely ignored: a callback that returns
  // still woke the renderer every period. When the event loop is late the next
  // arm stretches instead of spending another git status on a machine that is
  // already behind. Background includes a blurred webview, not only a hidden one.
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let pollWanted = false;
  let unbindPollForeground: (() => void) | null = null;
  let pollInflight = false;
  /** Monotonic poll ordering; a superseded tick's result is discarded. */
  let pollSequenceSource = 0;
  const pollRuns = new Map<string, number>();
  /** Whether the quit-time persist flush is attached. It stays for the store's life. */
  let pagehideWired = false;

  // Monotonic token source for diff-selection requests. Session `generation`
  // only moves on tab activation, so it cannot order two rapid selections of
  // the same tab; this does.
  const selectionGeneration = beginGeneration();

  /**
   * Marks a diff fetch as in flight, but only when the target actually moves.
   *
   * The `select*Diff` calls are also how the app refetches the diff already
   * on screen after a mutation or a watcher event. Raising the flag for those
   * would blink a skeleton over content that is about to be replaced by
   * nearly the same content, several times a minute.
   */
  const beginDiffFetch = (
    session: RepoSession,
    target: {
      filePath: string | null;
      commitId: string | null;
      isStaged: boolean;
      ignoreWhitespace: boolean;
    },
  ): void => {
    const unchanged =
      session.selectedFilePath === target.filePath &&
      session.selectedCommitId === target.commitId &&
      session.selectedIsStaged === target.isStaged &&
      session.selectedIgnoreWhitespace === target.ignoreWhitespace &&
      session.selectedDiff !== null;
    if (unchanged) return;
    applyToSession(session.id, session.generation, { selectedDiffPending: true });
  };

  /**
   * How many poll ticks between re-asserting the active repository's watch.
   *
   * A watch can die AFTER it was established — the backend reaps a session
   * whose event stream closes, whose thread panics, or whose repository
   * disappears, and its own log says "UI refresh for this repo is dead until
   * it is re-watched". Nothing told the frontend, so the session went on
   * believing it was live and the compensating full poll never engaged.
   *
   * Re-asserting repairs rather than merely reports: `cmd_watch_repo` returns
   * immediately when the session is still registered, and creates a fresh
   * watcher when it was reaped. Every 10 ticks (~60s) keeps a dead watch's
   * blind window bounded to about a minute without putting a subprocess on
   * the 6-second path.
   */
  const WATCH_REASSERT_EVERY_TICKS = 10;
  let pollTickCount = 0;

  function onStatusPollVisibility(): void {
    if (!pollWanted) return;
    if (readBackgroundDocument()) {
      if (pollTimer !== null) {
        clearTimeout(pollTimer);
        pollTimer = null;
      }
      return;
    }
    armStatusPoll();
  }

  function bindStatusPollVisibility(): void {
    if (unbindPollForeground !== null || typeof document === "undefined") return;
    unbindPollForeground = bindForegroundChanges(
      document,
      typeof window === "undefined" ? null : window,
      onStatusPollVisibility,
    );
  }

  function unbindStatusPollVisibility(): void {
    unbindPollForeground?.();
    unbindPollForeground = null;
  }

  function armStatusPoll(): void {
    if (!pollWanted || pollTimer !== null || typeof setTimeout === "undefined") return;
    const decision = decideCadence({
      baseMs: STATUS_POLL_INTERVAL_MS,
      lagMs: readEventLoopDelay(),
      paused: readBackgroundDocument(),
    });
    if (!decision.run) return;
    pollTimer = setTimeout(() => {
      pollTimer = null;
      void runStatusPoll().finally(() => {
        if (pollWanted) armStatusPoll();
      });
    }, decision.delayMs);
  }

  function ensureStatusPoll() {
    if (pollWanted || typeof setTimeout === "undefined") return;
    pollWanted = true;
    bindStatusPollVisibility();
    armStatusPoll();
  }

  /** Stops the workspace poll; the next activation restarts it lazily. */
  function stopStatusPoll() {
    pollWanted = false;
    if (pollTimer !== null) {
      clearTimeout(pollTimer);
      pollTimer = null;
    }
    unbindStatusPollVisibility();
  }

  async function runStatusPoll() {
    const session = activeSession();
    const hidden = readBackgroundDocument();
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
    // A refused repository stays on the one error hydrate already recorded.
    // Polling it again would re-run Git and re-log the same refusal.
    if (session!.trustRequired) return;
    const path = session!.path;
    const generation = session!.generation;
    const sessionId = session!.id;

    // Periodically re-assert the watch so a watcher that died after startup
    // self-heals, and so its loss becomes visible if it cannot. Deliberately
    // not awaited: the status tick must not wait on it, and its own result is
    // generation-guarded before it lands.
    pollTickCount += 1;
    if (pollTickCount % WATCH_REASSERT_EVERY_TICKS === 0) {
      void watch(path).then((state) => {
        applyToSession(sessionId, generation, { watch: state });
      });
    }

    // A repository with no live watcher gets a FULL refresh on this tick
    // instead of the statuses-only one. The watcher is what refreshes
    // branches, the graph, the parked-operation banner and the stash stack on
    // a tab the user is already sitting on; without it those go stale forever
    // while the file list keeps updating, which reads as "everything is
    // current". This is the compensation that makes the indicator honest
    // rather than merely apologetic. Bounded to the ACTIVE session, so the
    // extra cost is one snapshot per interval and only while degraded.
    if (needsFullPoll(session!.watch)) {
      pollInflight = true;
      try {
        await hydrate(sessionId, path, generation);
      } finally {
        pollInflight = false;
      }
      return;
    }
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
  /** Last open-tab set synced into workspace.json — skip no-op publishes. */
  let lastWorkspaceSyncKey: string | null = null;
  let persistTimer: ReturnType<typeof setTimeout> | null = null;
  /**
   * User edits that may shrink the tab list or clear groups advance this.
   * Restoring a session does not: a half-finished restore must not be able
   * to replace the durable copy.
   */
  let workspaceEpoch = 0;
  /** Paths closed since the last successful save. Coalesce honors these drops. */
  let droppedPaths: string[] = [];
  /** Non-zero while restore is applying the durable copy. Writes wait. */
  let persistSuspended = 0;

  function familyOfTab(id: string) {
    return familyFromCommonDir(internal.sessions[id]?.commonDir, options);
  }

  /**
   * Moves a newly opened checkout to just after the last open checkout of the
   * same repository in the same group. Without it an agent's worktree tab
   * landed at the far end of the strip, as far from its repository as the
   * strip allowed, and with stacking turned off nothing tied the two together.
   */
  function placeBesideFamily(id: string, commonDir: string | null | undefined) {
    const family = familyFromCommonDir(commonDir, options);
    if (!family) return;
    const ws = internal.workspace;
    const from = ws.tabs.findIndex((tab) => tab.id === id);
    if (from < 0) return;
    const group = ws.tabs[from].group ?? null;
    let last = -1;
    ws.tabs.forEach((tab, index) => {
      if (tab.id === id || (tab.group ?? null) !== group) return;
      if (familyOfTab(tab.id)?.key === family.key) last = index;
    });
    if (last < 0) return;
    replaceWorkspace(moveWorkspaceTabTo(ws, id, from > last ? last + 1 : last));
  }

  function beginShortcut(): boolean {
    if (shortcutLocked) return false;
    shortcutLocked = true;
    queueMicrotask(() => {
      shortcutLocked = false;
    });
    return true;
  }

  /** How the strip stacks worktrees right now; the one input the bar and cycling share. */
  function stripStacking(): StackingOptions {
    return {
      enabled: get(interfaceStore).stackWorktreeTabs,
      expanded: get(expandedStacks),
      lastUsed: lastUsedCheckouts(),
      identity: (path: string) => identityKey(path, options),
    };
  }

  /** The strip as drawn, for shortcuts that step through what the reader sees. */
  function drawnLayout() {
    const projected = project(internal, options);
    return computeTabLayout(
      projected.openTabs,
      projected.collapsedGroups,
      undefined,
      projected.groupColors,
      stripStacking(),
    );
  }

  function publish() {
    const activeId = internal.workspace.activeId;
    if (activeId) noteActiveCheckout(familyOfTab(activeId)?.key, activeId);
    set(project(internal, options));
    persist();
    const active = internal.workspace.activeId
      ? internal.sessions[internal.workspace.activeId]
      : undefined;
    const trusted = pathsTrustedForBackground(
      internal.workspace.tabs.map((tab) => ({
        path: tab.path,
        trustRequired: internal.sessions[tab.id]?.trustRequired === true,
      })),
      active?.path ?? null,
    );
    const syncKey = JSON.stringify(trusted);
    if (syncKey !== lastWorkspaceSyncKey) {
      lastWorkspaceSyncKey = syncKey;
      workspaceSync.scheduleWorkspaceSync(trusted.activeKey, trusted.retainedKeys);
      const visible = !readBackgroundDocument();
      const scope = {
        activeKey: trusted.activeKey,
        retainedKeys: trusted.retainedKeys,
        visible,
      };
      autoInit.setScope(scope);
      liveIndex.setScope(scope);
    }
  }

  /**
   * Records a user edit that is allowed to shrink the durable tab list.
   * Paths named here stay closed; every other tab the snapshot forgot is
   * put back by coalesce.
   */
  function commitEdit(closed: readonly string[] = []) {
    workspaceEpoch += 1;
    for (const path of closed) {
      if (path && !droppedPaths.includes(path)) droppedPaths.push(path);
    }
    if (droppedPaths.length > MAX_OPEN_TABS * 2) {
      droppedPaths = droppedPaths.slice(-MAX_OPEN_TABS * 2);
    }
  }

  /**
   * Persists workspace state. The write + menu IPC are debounced on a short
   * trailing delay so per-keystroke state (commit draft, search query) cannot
   * storm localStorage and the native menu; `flushPersist` runs the live
   * snapshot immediately whenever a critical mutation lands.
   */
  function persist() {
    if (persistSuspended > 0) return;
    if (persistTimer !== null || typeof setTimeout === "undefined") return;
    persistTimer = setTimeout(() => flushPersist(), PERSIST_DEBOUNCE_MS);
  }

  function flushPersist(force = false) {
    if (persistTimer !== null) {
      clearTimeout(persistTimer);
      persistTimer = null;
    }
    if (persistSuspended > 0) return;
    const data = workspaceToPersisted(internal.workspace, internal.sessions, workspaceEpoch);
    const recents = internal.workspace.recents.slice(0, MENU_RECENT_CAP);
    const payload = JSON.stringify({ data, recents, dropped: droppedPaths });
    if (!force && payload === lastPersistedPayload) return;
    const closed = droppedPaths.slice();
    if (!savePersistedWorkspace(storage, data, closed, options)) {
      // The write did not land (quota, private mode). Leave the dedup marker
      // clear so the same snapshot is retried instead of being skipped.
      lastPersistedPayload = null;
      if (typeof setTimeout !== "undefined" && persistTimer === null) {
        persistTimer = setTimeout(() => flushPersist(), PERSIST_DEBOUNCE_MS);
      }
    } else {
      lastPersistedPayload = payload;
      droppedPaths = [];
    }
    const recentsJson = JSON.stringify(recents);
    if (recentsJson !== lastSentRecentsJson) {
      lastSentRecentsJson = recentsJson;
      void invokeFn("cmd_set_recent_menu", { paths: recents }).catch((error) => {
        if (lastSentRecentsJson === recentsJson) lastSentRecentsJson = null;
        diagnostics.warn("desktop:recent-menu", error);
      });
    }
  }

  function installQuitFlush() {
    if (pagehideWired || typeof document === "undefined") return;
    document.addEventListener("pagehide", () => flushPersist(true));
    pagehideWired = true;
  }
  installQuitFlush();

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
      // `operation` is the one object-valued field, and the snapshot rebuilds
      // it every poll — reference equality would republish the whole store to
      // every subscriber every six seconds on a repository where nothing
      // happened. Compared through its owner rather than a generic deep-equal,
      // so no other field silently acquires expensive comparison semantics.
      if (key === "watch") {
        if (watchStatesEqual(session.watch, incoming as unknown as WatchState)) {
          continue;
        }
        return false;
      }
      if (key === "operation") {
        if (
          operationStatesEqual(
            session.operation,
            incoming as unknown as OperationState,
          )
        ) {
          continue;
        }
        return false;
      }
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
    /**
     * A patch, or a function given the LIVE session that returns one.
     *
     * The function form exists for fields that merge rather than replace —
     * `viewSections` above all. A caller that spreads a session snapshot
     * captured before an await would silently undo any section change made
     * while its fetch was in flight; reading the live session here cannot.
     */
    patchOrFn: Partial<RepoSession> | ((current: RepoSession) => Partial<RepoSession>),
  ) {
    const session = internal.sessions[id];
    if (!session || session.generation !== generation) return false;
    const patch = typeof patchOrFn === "function" ? patchOrFn(session) : patchOrFn;
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
    operation: OperationState;
    stashEntries: StashEntry[];
    stashFailed: boolean;
    stashTruncated: boolean;
    tagsTruncated: boolean;
    tagsFailed: boolean;
    fetchedAt: number | null;
  }> {
    const [branches, statuses, tags, operation, stash, fetchedAt] = await Promise.all([
      invokeFn<BranchInfo[]>("cmd_list_branches", { repoPath: path }),
      invokeFn<FileStatus[]>("cmd_get_status", { repoPath: path }),
      invokeFn<unknown>("cmd_list_tags", { repoPath: path })
        .then((raw) => parseTagList(raw))
        .catch(() => ({ tags: [] as TagInfo[], truncated: false, failed: true })),
      // A failed probe is recorded as a failure, never folded into "idle".
      // Reporting "no operation in progress" because the check itself broke
      // is what strands a user mid-merge in a UI insisting all is well. It
      // does not fail the snapshot, though: branches and statuses are still
      // worth rendering, and the marker says the state is unknown.
      invokeFn<RepoOperation | null>("cmd_repo_operation", { repoPath: path })
        .then((value) => ({ operation: value ?? null, probeFailed: false }))
        .catch(() => ({ operation: null, probeFailed: true })),
      // Same fail-soft-but-honest treatment as the operation probe: an empty
      // stash list and an unreadable one must not render the same, because a
      // forgotten stash is work that exists nowhere else.
      invokeFn<unknown>("cmd_stash_list", { repoPath: path })
        .then((raw) => ({ ...parseStashList(raw), failed: false }))
        .catch(() => ({ entries: [] as StashEntry[], failed: true, truncated: false })),
      invokeFn<number | null>("cmd_last_fetch_at", { repoPath: path })
        .then((value) => (typeof value === "number" ? value : null))
        .catch(() => null),
    ]);
    const currentBranch = branches.find((b) => b.is_current)?.name || null;
    const defaultBranch =
      branches.find((b) => b.is_default)?.name || currentBranch || "main";
    return {
      branches,
      statuses,
      tags: tags.tags,
      currentBranch,
      defaultBranch,
      operation,
      stashEntries: stash.entries,
      stashFailed: stash.failed,
      stashTruncated: stash.truncated,
      tagsTruncated: tags.truncated,
      tagsFailed: tags.failed,
      fetchedAt,
    };
  }

  /**
   * Starts the filesystem watch and REPORTS the outcome.
   *
   * Still best-effort in the sense that a failure never blocks opening a
   * repository — but the failure is no longer invisible. `cmd_watch_repo`
   * fails for ordinary reasons (the backend's watch table is full, the
   * platform refuses another inotify handle), and a repository with no watcher
   * silently stops receiving branch, graph, operation and stash updates while
   * looking exactly like one that is live.
   */
  async function watch(path: string): Promise<WatchState> {
    try {
      await invokeFn("cmd_watch_repo", { repoPath: path });
      return WATCH_ACTIVE;
    } catch (err: unknown) {
      return watchFailed(err);
    }
  }

  /**
   * Tells the backend the user just brought `path` to the front, so the
   * hydrate that follows is admitted like the user action it is instead of
   * queuing behind background refreshes (`cmd_note_tab_activated`). Only for
   * activations a person caused: restore and background opens stay ordinary
   * reads, or a restart would hand every restored tab a free burst.
   *
   * Best effort. A refusal here is the same refusal the hydrate meets next,
   * and the hydrate is what reports it.
   */
  async function noteTabActivated(path: string): Promise<void> {
    try {
      await invokeFn("cmd_note_tab_activated", { repoPath: path });
    } catch {
      /* reported by the hydrate that follows */
    }
  }

  async function unwatch(path: string) {
    // A refresh still owed to a repo nobody watches would run against a
    // session that is gone, or worse, one reopened since.
    watcherRefreshPolicy.forget(path);
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
      operation: OperationState;
      stashEntries: StashEntry[];
      stashFailed: boolean;
      stashTruncated: boolean;
      tagsTruncated: boolean;
      tagsFailed: boolean;
      fetchedAt: number | null;
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
        trustRequired: false,
        hasHydrated: true,
      });
      // Stats re-drain even when the snapshot was a no-op: a previously
      // failed drain must retry on the next refresh, and the backend
      // memoizes per-tip so an unchanged repo costs one cheap call.
      if (internal.sessions[id]?.generation === generation) {
        contentRevisions.update((revisions) => ({
          ...Object.fromEntries(Object.values(internal.sessions)
            .filter((session) => revisions[session.path] !== undefined)
            .map((session) => [session.path, revisions[session.path]])),
          [path]: `${generation}:${run}`,
        }));
        void fetchBranchStats(id, path, generation);
      }
    } catch (err: unknown) {
      if (snapshotRuns.get(id) !== run) return;
      const message = formatError(err);
      // A deferral reaching here has already been retried through the whole
      // backoff by src/lib/ipc/invoke.ts. The snapshot already rendered stays;
      // the toast and diagnostics show the deferral as a warning, not an error.
      applyToSession(id, generation, {
        isLoading: false,
        error: message,
        ...(repositoryTrustRefused(message) ? { trustRequired: true } : {}),
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

  // Which watcher events may cost a refresh, and when: the active tab of a
  // shown window at once, background tabs on a bounded schedule, nothing while
  // hidden. See watcherRefresh.ts for why the cost had to stop scaling with
  // the number of busy tabs.
  const watcherRefreshPolicy = createWatcherRefreshPolicy({
    now: () => Date.now(),
    setTimer: (run, delayMs) => setTimeout(run, delayMs),
    clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    isActive: (path) => {
      const active = activeSession();
      return !!active && sameRepo(active.path, path, options);
    },
    isHidden: readHiddenDocument,
    refresh: (path) => void store.refresh(path),
  });
  let unbindRefreshVisibility: (() => void) | null = null;
  function bindRefreshVisibility(): void {
    if (unbindRefreshVisibility !== null || typeof document === "undefined") return;
    unbindRefreshVisibility = bindForegroundChanges(
      document,
      typeof window === "undefined" ? null : window,
      () => watcherRefreshPolicy.onVisibilityChange(),
    );
  }

  const mutationActivity = writable<Record<string, string[]>>({});
  const mutations = new Map<number, { path: string; kind: string }>();
  let mutationSequence = 0;
  function beginMutation(path: string, kind: string): () => void {
    const token = ++mutationSequence;
    const publishActivity = () => {
      const activity: Record<string, string[]> = {};
      for (const entry of mutations.values()) {
        (activity[entry.path] ??= []).push(entry.kind);
      }
      mutationActivity.set(activity);
    };
    mutations.set(token, { path, kind });
    publishActivity();
    return () => { mutations.delete(token); publishActivity(); };
  }

  const store = {
    subscribe,
    mutationActivity: { subscribe: mutationActivity.subscribe },
    contentRevisions: { subscribe: contentRevisions.subscribe },
    setError: (error: string | null) => {
      // Every user-facing error funnels through here; mirror it into the
      // diagnostics log so the banner's dismissal never loses it.
      if (error) diagnostics.error("repo", error);
      const session = activeSession();
      // Publishing an unchanged error retriggers every $repoStore subscriber,
      // including App's forwarding $effect that calls setError to clear —
      // which is the freeze that shipped as effect_update_depth_exceeded.
      if (
        internal.workspaceError === error &&
        (session === undefined || session.error === error)
      ) {
        return;
      }
      if (session) {
        putSession({ ...session, error });
      }
      internal = { ...internal, workspaceError: error };
      publish();
    },
    trustRepo: (path: string) => requestRepositoryTrust(path, "Trust Repository", invokeFn),
    openRepo: async (
      rawPath: string,
      extras: {
        allowBroken?: boolean;
        /** Runs only while this successfully hydrated open still owns navigation. */
        onReady?: (canonicalPath: string) => void;
        activate?: boolean;
        pinned?: boolean;
        /** Set when opening a tab that already belongs to a group. */
        group?: string | null;
        /** Own tab color. Omitted leaves whatever the open tab already has. */
        color?: TabColor | null;
        /** Restore must not advance the epoch; a partial walk cannot shrink the saved list. */
        keepEpoch?: boolean;
        /**
         * Keep the tab when the repository is not trusted yet, without
         * prompting. Restore uses this so closing the app cannot drop every
         * repository that would have asked.
         */
        deferTrust?: boolean;
        restore?: {
          viewTab?: ViewTab;
          viewSections?: Record<string, string>;
          searchQuery?: string;
          selectedBranch?: string | null;
          terminalOpen?: boolean;
        };
      } = {},
    ) => {
      // Only an open that may take the screen is navigation. A background
      // open (a task agent's checkout, a restored tab) bumping the epoch
      // cancelled whatever the reader was opening at that moment.
      const requestId = extras.activate === false ? openEpoch : ++openEpoch;
      internal = { ...internal, workspaceError: null };
      let resolved: ResolvedRepo | null = null;
      let deferredTrustMessage: string | null = null;
      try {
        resolved = await resolvePath(rawPath);
      } catch (err: unknown) {
        const message = formatError(err);
        const refused = repositoryTrustRefused(message);
        if (refused && !extras.deferTrust) {
          try {
            const trustedPath = await requestRepositoryTrust(rawPath, "Trust and Open", invokeFn);
            if (!trustedPath) return false;
            resolved = await resolvePath(trustedPath);
          } catch (trustError: unknown) {
            internal = { ...internal, workspaceError: formatError(trustError) };
            publish();
            return false;
          }
        } else if (refused) {
          deferredTrustMessage = message;
        } else if (!extras.allowBroken) {
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
        ...(extras.group !== undefined ? { group: extras.group } : {}),
        ...(extras.color !== undefined ? { color: extras.color } : {}),
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
      if (!extras.keepEpoch && opened.created) commitEdit();
      replaceWorkspace({
        ...opened.workspace,
        lastClosed: opened.workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      // A restore keeps the order it was saved in; anything else opened
      // beside an open checkout of the same repository lands next to it.
      if (opened.created && !extras.keepEpoch) {
        placeBesideFamily(opened.id, resolved?.common_dir);
      }
      const existing = internal.sessions[opened.id];
      const session = existing
        ? {
            ...bumped(existing),
            path,
            name: resolved?.name ?? existing.name,
            isBare: resolved?.is_bare ?? existing.isBare,
            // A fresh resolve is the answer, including "could not read it";
            // a failed one keeps what the last good resolve said.
            commonDir: resolved ? resolved.common_dir ?? null : existing.commonDir,
            pinned:
              opened.workspace.tabs.find((tab) => tab.id === opened.id)
                ?.pinned ?? existing.pinned,
            isLoading: resolved ? !existing.hasHydrated : false,
            error: resolved
              ? null
              : deferredTrustMessage ??
                String(internal.workspaceError ?? "Repository is unavailable"),
            trustRequired: resolved
              ? false
              : deferredTrustMessage !== null || existing.trustRequired,
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
              commonDir: resolved?.common_dir ?? null,
              // An adopted alias tab hands over its state; an explicit
              // restore payload always wins over what the alias carried.
              activeTab: extras.restore?.viewTab ?? carriedSession?.activeTab,
              viewSections:
                extras.restore?.viewSections ?? carriedSession?.viewSections,
              searchQuery:
                extras.restore?.searchQuery ?? carriedSession?.searchQuery,
              selectedBranch:
                extras.restore?.selectedBranch ??
                carriedSession?.selectedBranch,
              terminalOpen:
                extras.restore?.terminalOpen ?? carriedSession?.terminalOpen,
              commitDraft: carriedSession?.commitDraft ?? "",
              isAmending: carriedSession?.isAmending ?? false,
              isLoading: Boolean(resolved),
              error: resolved
                ? null
                : deferredTrustMessage ?? "Repository is unavailable",
              trustRequired: deferredTrustMessage !== null,
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
      // Recorded before the hydrate so the first snapshot already carries an
      // honest live/degraded answer, rather than briefly claiming live updates
      // for a repository that never got a watcher.
      const watchState = await watch(path);
      applyToSession(opened.id, session.generation, { watch: watchState });
      if (shouldPresent && !extras.keepEpoch) await noteTabActivated(path);
      await hydrate(opened.id, path, session.generation);
      const latest = internal.sessions[opened.id];
      if (
        shouldPresent &&
        latest &&
        internal.workspace.activeId === opened.id
      ) {
        revealGraph(latest);
      }
      if (shouldPresent && requestId === openEpoch && latest &&
          latest.generation === session.generation && !latest.error &&
          latest.activeTab === session.activeTab && latest.viewSections === session.viewSections &&
          latest.selectedFilePath === session.selectedFilePath && latest.selectedCommitId === session.selectedCommitId &&
          latest.selectedDiffPending === session.selectedDiffPending &&
          internal.workspace.activeId === opened.id) extras.onReady?.(path);
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
    revokeTrust: async (id: string) => {
      const session = internal.sessions[id];
      if (!session) return;
      const approved = await askConfirm({
        title: "Revoke repository trust?",
        message: `${session.path}\n\nThis closes the tab and blocks new GitPulse operations. Trust was granted to the repository, so this revokes it for every worktree of it, not only this checkout: any other tab open on the same repository stays open but stops working until it is trusted again. Already-running terminals and agent tasks retain their permissions; stop them separately if needed.`,
        confirmLabel: "Revoke Trust",
        destructive: true,
      });
      if (!approved) return;
      try {
        await invokeFn("cmd_revoke_repository_trust", { repoPath: session.path });
        await store.closeTab(id);
      } catch (error: unknown) {
        store.setError(formatError(error));
      }
    },
    activateTab: async (id: string, extras: { force?: boolean } = {}) => {
      openEpoch += 1;
      const current = internal.sessions[id];
      // Restore passes force and must not prompt. A click on a tab whose
      // hydrate or open was refused is the moment the person can approve it,
      // including when that tab is already the active one.
      if (!extras.force && current?.trustRequired) {
        if (internal.workspace.activeId !== id) {
          const next = activateTab(internal.workspace, id);
          if (next === internal.workspace) return;
          replaceWorkspace(next);
        }
        syncFilterFromSession(current);
        publish();
        const trustedPath = await requestRepositoryTrust(current.path, "Trust and Open", invokeFn);
        const live = internal.sessions[id];
        if (!live?.trustRequired) return;
        if (!trustedPath) return;
        let path = trustedPath;
        try {
          const resolved = await resolvePath(trustedPath);
          path = resolved.path;
        } catch (err: unknown) {
          const message = formatError(err);
          applyToSession(id, live.generation, {
            isLoading: false,
            error: message,
            trustRequired: repositoryTrustRefused(message) || live.trustRequired,
          });
          return;
        }
        const latest = internal.sessions[id];
        if (!latest) return;
        const activation = bumped({ ...latest, path });
        putSession({ ...activation, path, isLoading: true });
        if (internal.workspace.activeId === id) {
          syncFilterFromSession(activation);
          revealGraph(activation);
        }
        publish();
        const watchState = await watch(path);
        applyToSession(id, activation.generation, { watch: watchState });
        watcherRefreshPolicy.onActivated(path);
        await noteTabActivated(path);
        await hydrate(id, path, activation.generation);
        const after = internal.sessions[id];
        if (after && after.generation === activation.generation && !after.trustRequired) {
          ensureStatusPoll();
          flushPersist();
        }
        return;
      }
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
      if (session.trustRequired) {
        syncFilterFromSession(session);
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
      watcherRefreshPolicy.onActivated(session.path);
      // `force` is restore presenting a saved tab, not a person clicking it.
      if (!extras.force) await noteTabActivated(session.path);
      await hydrate(id, session.path, activation.generation);
      ensureStatusPoll();
      flushPersist();
    },
    closeTab: async (id: string) => {
      const session = internal.sessions[id];
      // Closing the tab unmounts its terminal panel, which kills every shell
      // in it. Ask first, and only when there is something to lose — a
      // confirmation on every close would train the user to dismiss it, which
      // is how the one that mattered gets dismissed too.
      if (session && !(await confirmTerminalLoss([session.path]))) return;
      const result = closeTab(internal.workspace, id);
      if (result.reason === "missing") return;
      commitEdit(result.closedPath ? [result.closedPath] : []);
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
      if (!(await confirmTerminalLoss(removed.map((tab) => tab.path)))) return;
      commitEdit(removed.map((tab) => tab.path));
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
      if (!(await confirmTerminalLoss(removed.map((tab) => tab.path)))) return;
      commitEdit(removed.map((tab) => tab.path));
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
    // Next/previous and number keys step through the strip as drawn — a
    // folded worktree stack is one stop, a group's tabs sit together — so
    // the keyboard and the native menu land where the eye expects.
    nextTab: async () => {
      if (!beginShortcut()) return;
      const next = cycleStop(drawnLayout(), internal.workspace.activeId, 1);
      if (next && next !== internal.workspace.activeId) {
        await store.activateTab(next);
      }
    },
    prevTab: async () => {
      if (!beginShortcut()) return;
      const next = cycleStop(drawnLayout(), internal.workspace.activeId, -1);
      if (next && next !== internal.workspace.activeId) {
        await store.activateTab(next);
      }
    },
    /**
     * Applies an order the strip computed from what it draws. A regroup, when
     * given, moves those tabs into that group first (and opens it, so a drop
     * never makes the dropped tab vanish). Refused whole when the order is
     * not a permutation of the open tabs.
     */
    arrangeTabs: (orderedIds: readonly string[], regroup?: { ids: readonly string[]; group: string | null }) => {
      let next = internal.workspace;
      if (regroup) {
        for (const id of regroup.ids) next = setWorkspaceTabGroup(next, id, regroup.group);
        if (regroup.group) next = setWorkspaceGroupCollapsed(next, regroup.group, false);
      }
      const arranged = arrangeWorkspaceTabs(next, orderedIds);
      const alreadyInOrder =
        orderedIds.length === next.tabs.length && next.tabs.every((tab, i) => tab.id === orderedIds[i]);
      // A stale plan must not half-apply: refused order means no regroup either.
      if (arranged === next && !alreadyInOrder) return false;
      if (arranged === internal.workspace) return false;
      commitEdit();
      replaceWorkspace(arranged);
      publish();
      flushPersist();
      return true;
    },
    activateTabAt: async (index: number) => {
      if (!Number.isInteger(index) || index < 0) return;
      const id = activationStops(drawnLayout())[index];
      if (id) await store.activateTab(id);
    },
    // Moves step through the strip as drawn (see repos/stripNav.ts): a folded
    // worktree stack moves with every checkout in it, a group's tab stays in
    // its group, and a step never swaps with a tab the reader cannot see. The
    // tab bar, the command palette and the keyboard all come through here.
    moveTabBy: (id: string, delta: number): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnit(layout, unit, delta) : null;
      return order ? store.arrangeTabs(order) : false;
    },
    moveTabToEdge: (id: string, edge: "start" | "end"): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnitToEdge(layout, unit, edge) : null;
      return order ? store.arrangeTabs(order) : false;
    },
    canMoveTab: (id: string, delta: -1 | 1): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      return unit !== null && canMoveUnit(layout, unit, delta);
    },
    pinTab: (id: string, pinned: boolean) => {
      commitEdit();
      replaceWorkspace(pinWorkspaceTab(internal.workspace, id, pinned));
      const session = internal.sessions[id];
      if (session) putSession({ ...session, pinned });
      publish();
    },
    setTabGroup: (id: string, group: string | null) => {
      commitEdit();
      replaceWorkspace(setWorkspaceTabGroup(internal.workspace, id, group));
      publish();
      flushPersist();
    },
    setTabColor: (id: string, color: TabColor | null) => {
      const next = setWorkspaceTabColor(internal.workspace, id, color);
      if (next === internal.workspace) return;
      commitEdit();
      replaceWorkspace(next);
      publish();
      flushPersist();
    },
    setGroupColor: (groupName: string, color: TabColor | null) => {
      const next = setWorkspaceGroupColor(internal.workspace, groupName, color);
      if (next === internal.workspace) return;
      commitEdit();
      replaceWorkspace(next);
      publish();
      flushPersist();
    },
    groupByParentFolder: () => {
      commitEdit();
      replaceWorkspace(
        groupWorkspaceByParentFolder(internal.workspace, (tab) => familyOfTab(tab.id)?.root),
      );
      publish();
      flushPersist();
    },
    ungroupTabs: (groupName?: string) => {
      commitEdit();
      replaceWorkspace(ungroupWorkspaceTabs(internal.workspace, groupName));
      publish();
      flushPersist();
    },
    closeGroup: async (groupName: string) => {
      const groupTabs = internal.workspace.tabs.filter((t) => t.group === groupName);
      if (groupTabs.length === 0) return;
      if (!(await confirmTerminalLoss(groupTabs.map((t) => t.path)))) return;
      const { workspace, closedPaths } = closeWorkspaceGroup(internal.workspace, groupName);
      commitEdit(groupTabs.map((tab) => tab.path));
      replaceWorkspace(workspace);
      const remaining = new Set(internal.workspace.tabs.map((tab) => tab.id));
      const sessions: Record<string, RepoSession> = {};
      for (const [key, session] of Object.entries(internal.sessions)) {
        if (remaining.has(key)) sessions[key] = session;
      }
      internal = { ...internal, sessions };
      stopStatusPoll();
      for (const path of closedPaths) {
        graph.evict(path);
        await unwatch(path);
      }
      const revealed = activeSession();
      if (revealed) {
        putSession(bumped(revealed));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    renameGroup: (oldName: string, newName: string) => {
      commitEdit();
      replaceWorkspace(renameWorkspaceGroup(internal.workspace, oldName, newName));
      publish();
      flushPersist();
    },
    toggleGroupCollapsed: (groupName: string) => {
      commitEdit();
      replaceWorkspace(toggleWorkspaceGroupCollapsed(internal.workspace, groupName));
      publish();
      flushPersist();
    },
    setGroupCollapsed: (groupName: string, collapsed: boolean) => {
      commitEdit();
      replaceWorkspace(setWorkspaceGroupCollapsed(internal.workspace, groupName, collapsed));
      publish();
      flushPersist();
    },
    isGroupCollapsed: (groupName: string) => {
      return isWorkspaceGroupCollapsed(internal.workspace, groupName);
    },
    collapseAllGroups: () => {
      commitEdit();
      replaceWorkspace(collapseWorkspaceAllGroups(internal.workspace));
      publish();
      flushPersist();
    },
    expandAllGroups: () => {
      commitEdit();
      replaceWorkspace(expandWorkspaceAllGroups(internal.workspace));
      publish();
      flushPersist();
    },
    reopenLastClosed: async () => {
      const path = internal.workspace.lastClosed[0];
      if (!path) return;
      await store.openRepo(path, { activate: true });
    },
    clearRecents: () => {
      replaceWorkspace({ ...internal.workspace, recents: [] });
      publish();
      flushPersist();
    },
    removeRecent: (path: string) => {
      replaceWorkspace(
        removeWorkspaceRecent(internal.workspace, path, options),
      );
      publish();
    },
    removeRepo: async (path: string) => {
      const tab = internal.workspace.tabs.find((item) =>
        sameRepo(item.path, path, options),
      );
      if (tab) {
        await store.closeTab(tab.id);
      }
      replaceWorkspace({
        ...internal.workspace,
        recents: internal.workspace.recents.filter(
          (item) => !sameRepo(item, path, options),
        ),
        lastClosed: internal.workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      publish();
      flushPersist();
    },
    refresh: async (repoPath?: string) => {
      const session = repoPath
        ? Object.values(internal.sessions).find((item) =>
            sameRepo(item.path, repoPath, options),
          )
        : activeSession();
      if (!session || session.trustRequired) return;
      const generation = session.generation;
      // Spinner only while nothing is rendered; a rendered error stays until
      // this refresh's own outcome replaces or retires it (hydrate settle).
      applyToSession(session.id, generation, {
        isLoading: !session.hasHydrated,
      });
      await hydrate(session.id, session.path, generation);
      const latest = internal.sessions[session.id];
      if (latest && internal.workspace.activeId === session.id) {
        // Reload with the visible filter context: the store normalizes the
        // query and the backend applies every term, so the cached payload
        // always answers exactly the query the scheduler keys on.
        void graph.loadGraph(latest.path, latest.searchQuery, latest.selectedBranch);
      }
    },
    handleRepoChanged: async (changedPath?: string | null) => {
      // Watcher events arrive per file; a checkout or rebase fires many at
      // once. Each changed path collapses onto its own trailing window so a
      // storm becomes one refresh; explicit refresh() calls stay undelayed.
      if (!changedPath) {
        if (activeSession()?.trustRequired) return;
        scheduleWatcherRefresh("", () => void store.refresh());
        return;
      }
      const session = Object.values(internal.sessions).find((item) =>
        sameRepo(item.path, changedPath, options),
      );
      if (!session || session.trustRequired) return;
      // Drop echoes of our own recent writes: the explicit refresh() in
      // runMutating already fetched fresh state, so acting on the echo would
      // refresh the whole session twice per mutation. An unrelated external
      // change inside the window is picked up by the next poll or event.
      const echoUntil = mutationEchoUntil.get(session.path);
      if (echoUntil !== undefined && Date.now() < echoUntil) return;
      bindRefreshVisibility();
      watcherRefreshPolicy.onChange(session.path);
    },
    restoreWorkspace: async () => {
      stopStatusPoll();
      persistSuspended += 1;
      try {
        const persisted = loadPersistedWorkspace(storage, options);
        workspaceEpoch = persisted.epoch ?? 0;
        droppedPaths = [];
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
        // A tab that never resolved (not trusted, or missing) must not be
        // hydrated here. Hydration is a git command, and restore is not the
        // moment to ask for trust or to run hooks.
        const presentRestored = async (id: string) => {
          const session = internal.sessions[id];
          if (session?.error && !session.hasHydrated) {
            replaceWorkspace(activateTab(internal.workspace, id));
            syncFilterFromSession(session);
            publish();
            return;
          }
          await store.activateTab(id, { force: true });
        };

        for (const tab of ordered) {
          // Always append (activate: false) so restore cannot shuffle tab
          // order. Present the previously-active session as soon as that
          // iteration finishes — remaining tabs keep hydrating behind it.
          // keepEpoch + a suspended save: quitting halfway cannot replace
          // the durable list with the tabs opened so far.
          await store.openRepo(tab.path, {
            allowBroken: true,
            deferTrust: true,
            keepEpoch: true,
            activate: false,
            pinned: tab.pinned,
            group: tab.group ?? null,
            ...(tab.color ? { color: tab.color } : {}),
            restore: {
              viewTab: tab.viewTab,
              viewSections: tab.viewSections,
              searchQuery: tab.searchQuery,
              selectedBranch: tab.selectedBranch,
              terminalOpen: tab.terminalOpen,
            },
          });
          if (!activated && isActive(tab.path)) {
            activated = true;
            const sessionTab = internal.workspace.tabs.find((item) =>
              sameRepo(item.path, tab.path, options),
            );
            if (sessionTab) {
              await presentRestored(sessionTab.id);
            }
          }
        }
        // Applied after every tab exists, so a collapsed group is not
        // discarded for having no members yet. The active tab's group was
        // expanded when that tab was presented; putting the saved list back
        // wholesale would collapse it again.
        const activeGroup = activated
          ? internal.workspace.tabs.find((tab) => tab.id === internal.workspace.activeId)?.group
          : null;
        const collapsedGroups = (persisted.collapsedGroups ?? []).filter(
          (group) => !activeGroup || group !== activeGroup,
        );
        replaceWorkspace(
          restoreGroupColors(
            { ...internal.workspace, collapsedGroups },
            persisted.groupColors,
          ),
        );
        publish();
        if (!activated) {
          const desired = persisted.activePath
            ? internal.workspace.tabs.find((tab) =>
                sameRepo(tab.path, persisted.activePath ?? "", options),
              )
            : internal.workspace.tabs[0];
          if (desired) {
            await presentRestored(desired.id);
          } else {
            syncFilterFromSession(undefined);
            graph.showRepo(null);
            publish();
          }
        }
      } finally {
        persistSuspended -= 1;
        flushPersist(true);
      }
    },
    /** Writes the live workspace immediately. Quit calls this before the process exits. */
    flushPersistedWorkspace: () => {
      flushPersist(true);
    },
    /** All uncommitted-change entry points share the existing file/diff view. */
    previewUncommitted: async (repoPath?: string, isStaged?: boolean): Promise<void> => {
      const session = activeSession();
      const target = repoPath ?? session?.path;
      if (!target) return;
      if (!session || !sameRepo(session.path, target, options) || session.isLoading || session.error) {
        let preview: Promise<void> | undefined;
        await store.openRepo(target, {
          onReady: () => { preview = store.previewUncommitted(undefined, isStaged); },
        });
        await preview;
        return;
      }
      const file = session.statuses.find(status => isStaged === undefined || (isStaged ? status.is_staged : hasUnstagedChanges(status)))
        ?? session.statuses[0];
      const side = isStaged === false && file && hasUnstagedChanges(file) ? false : file?.is_staged ?? false;
      openEpoch += 1;
      // Invalidate late commit/range reads even when this worktree became clean.
      selectionGeneration.next();
      applyToSession(session.id, session.generation, {
        selectedFilePath: file?.path ?? null,
        selectedCommitId: null,
        selectedDiff: null,
        selectedDiffTruncated: false,
        selectedDiffTruncationReason: null,
        selectedDiffPending: Boolean(file),
        selectedIsStaged: side,
        selectionKind: "file",
        activeTab: "history",
        viewSections: { ...session.viewSections, history: "diff" },
      });
      interfaceStore.setFleetOpen(false);
      if (file) await store.selectFileDiff(file.path, side);
    },
    selectFileDiff: async (filePath: string, isStaged: boolean = false) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      const navigationEpoch = openEpoch;
      // The whitespace preference lives on the session, not on call sites:
      // every refetch of this diff must carry whatever the user last chose.
      const ignoreWhitespace = session.selectedIgnoreWhitespace;
      const token = selectionGeneration.next();
      beginDiffFetch(session, {
        filePath,
        commitId: null,
        isStaged,
        ignoreWhitespace,
      });
      try {
        const diff = await invokeFn<DiffPayload>("cmd_get_file_diff", {
          repoPath: session.path,
          filePath,
          isStaged,
          ignoreWhitespace,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, (current) => ({
          selectedFilePath: filePath,
          selectedCommitId: null,
          selectedDiff: diff.text,
          selectedDiffTruncated: diff.truncated,
          selectedDiffTruncationReason: diff.truncation_reason ?? null,
          selectedIsStaged: isStaged,
          selectedIgnoreWhitespace: ignoreWhitespace,
          selectedDiffPending: false,
          selectionKind: "file",
          ...(navigationEpoch === openEpoch ? {
            activeTab: "history" as const,
            viewSections: { ...current.viewSections, history: "diff" },
          } : {}),
        }));
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        // The flag clears on failure too: a fetch that errored is not still
        // running, and leaving a spinner up would report a dead read as a
        // slow one.
        applyToSession(session.id, generation, {
          error: formatError(err),
          selectedDiffPending: false,
        });
      }
    },
    selectFilePath: (filePath: string | null) => {
      const session = activeSession();
      if (!session) return;
      // Records the shared file selection WITHOUT fetching a diff or moving
      // tabs: Coverage's file list and Blame's explorer converge on this one
      // site so the selection survives tab switches, and whichever viewer is
      // open reacts through its own effect. The diff fetch remains
      // selectFileDiff's job. Null clears the selection when the final editor
      // tab closes, preventing that closed path from reopening on remount.
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
      beginDiffFetch(session, {
        filePath: null,
        commitId,
        isStaged: false,
        ignoreWhitespace: session.selectedIgnoreWhitespace,
      });
      try {
        const diff = await invokeFn<DiffPayload>("cmd_get_commit_diff", {
          repoPath: session.path,
          commitId,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, {
          selectedCommitId: commitId,
          selectedFilePath: null,
          selectedDiff: diff.text,
          selectedDiffTruncated: diff.truncated,
          selectedDiffTruncationReason: diff.truncation_reason ?? null,
          selectedIsStaged: false,
          selectedDiffPending: false,
          selectionKind: "commit",
        });
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        // The flag clears on failure too: a fetch that errored is not still
        // running, and leaving a spinner up would report a dead read as a
        // slow one.
        applyToSession(session.id, generation, {
          error: formatError(err),
          selectedDiffPending: false,
        });
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
      beginDiffFetch(session, {
        filePath,
        commitId,
        isStaged: false,
        ignoreWhitespace: session.selectedIgnoreWhitespace,
      });
      try {
        const fileDiff = await invokeFn<DiffPayload>("cmd_get_commit_file_diff", {
          repoPath: session.path,
          commitId,
          filePath,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, (current) => ({
          selectedCommitId: commitId,
          selectedFilePath: filePath,
          selectedDiff: fileDiff.text,
          selectedDiffTruncated: fileDiff.truncated,
          selectedDiffTruncationReason: fileDiff.truncation_reason ?? null,
          selectedIsStaged: false,
          selectedDiffPending: false,
          selectionKind: "commit",
          activeTab: "history",
          viewSections: { ...current.viewSections, history: "diff" },
        }));
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        // The flag clears on failure too: a fetch that errored is not still
        // running, and leaving a spinner up would report a dead read as a
        // slow one.
        applyToSession(session.id, generation, {
          error: formatError(err),
          selectedDiffPending: false,
        });
      }
    },
    selectRangeDiff: async (from: string, to: string) => {
      const session = activeSession();
      if (!session) return;
      const generation = session.generation;
      const token = selectionGeneration.next();
      beginDiffFetch(session, {
        filePath: `${from}...${to}`,
        commitId: null,
        isStaged: false,
        ignoreWhitespace: session.selectedIgnoreWhitespace,
      });
      try {
        const diff = await invokeFn<DiffPayload>("cmd_get_range_diff", {
          repoPath: session.path,
          from,
          to,
        });
        if (!selectionGeneration.isCurrent(token)) return;
        applyToSession(session.id, generation, (current) => ({
          selectedFilePath: `${from}...${to}`,
          selectedCommitId: null,
          selectedDiff: diff.text,
          selectedDiffTruncated: diff.truncated,
          selectedDiffTruncationReason: diff.truncation_reason ?? null,
          selectedIsStaged: false,
          selectedDiffPending: false,
          selectionKind: "range",
          activeTab: "history",
          viewSections: { ...current.viewSections, history: "diff" },
        }));
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        // The flag clears on failure too: a fetch that errored is not still
        // running, and leaving a spinner up would report a dead read as a
        // slow one.
        applyToSession(session.id, generation, {
          error: formatError(err),
          selectedDiffPending: false,
        });
      }
    },
    stageFile: async (filePath: string) =>
      runMutating("stage", filePath, (path) =>
        invokeFn("cmd_stage_file", { repoPath: path, filePath }),
      ),
    unstageFile: async (filePath: string) => {
      const session = activeSession();
      if (!session) return { ok: false, error: "No active repository" };
      const file = session.statuses.find((entry) => entry.path === filePath && entry.is_staged);
      const filePaths = file ? indexSelectionPaths([file], "unstage") : [filePath];
      return runMutating("unstage", filePath, (path) => filePaths.length > 1
        ? invokeFn("cmd_change_index", { repoPath: path, filePaths, action: "unstage" })
        : invokeFn("cmd_unstage_file", { repoPath: path, filePath }), { session });
    },
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
    stageAll: () => runStageBatch("stage"),
    unstageAll: () => runStageBatch("unstage"),
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
    // --- stash ----------------------------------------------------------

    /**
     * Applies, pops, or drops a stash entry.
     *
     * Takes the entry rather than an index so the object id it was listed with
     * always travels with it: the backend re-resolves the index under its lock
     * and refuses the pair on a mismatch, which is what keeps a stale list from
     * dropping a stash that someone else pushed in the meantime.
     */
    stashAction: async (action: StashAction, entry: StashEntry) =>
      runMutating(`stash-${action}`, entry.selector, (path) =>
        invokeFn("cmd_stash_action", {
          repoPath: path,
          action,
          index: entry.index,
          expectedOid: entry.oid,
        }),
      ),

    /** The diff a stash entry holds, addressed by object id. */
    stashShow: async (oid: string): Promise<DiffPayload> => {
      const session = activeSession();
      if (!session) throw new Error("No repository is open.");
      return invokeFn<DiffPayload>("cmd_stash_show", { repoPath: session.path, oid });
    },

    // --- replaying and rewinding commits --------------------------------

    /**
     * Replays commits onto the current branch. A conflict parks the repository,
     * which the operation banner then offers a way out of.
     */
    cherryPick: async (commits: string[], noCommit = false) =>
      runMutating("cherry-pick", commits.join(", "), (path) =>
        invokeFn("cmd_cherry_pick", { repoPath: path, commits, noCommit }),
      ),

    /** Records the inverse of the given commits as new commits. */
    revertCommits: async (commits: string[], noCommit = false) =>
      runMutating("revert", commits.join(", "), (path) =>
        invokeFn("cmd_revert", { repoPath: path, commits, noCommit }),
      ),

    /**
     * Moves the current branch to `target`.
     *
     * `"hard"` destroys uncommitted work irrecoverably; callers are expected to
     * have confirmed with the user first — this is the last layer that could,
     * and it deliberately does not second-guess an explicit instruction.
     */
    resetTo: async (mode: ResetMode, target: string) =>
      runMutating(`reset-${mode}`, target, (path) =>
        invokeFn("cmd_reset", { repoPath: path, mode, target }),
      ),

    // --- remotes and submodules ----------------------------------------

    listRemotes: async (): Promise<{ remotes: RemoteInfo[]; truncated: boolean }> => {
      const session = activeSession();
      if (!session) return { remotes: [], truncated: false };
      const raw = await invokeFn<unknown>("cmd_list_remotes", { repoPath: session.path });
      const parsed = parseRemoteList(raw);
      if (parsed.failed) {
        throw new Error("The remote list was not readable.");
      }
      return { remotes: parsed.remotes, truncated: parsed.truncated };
    },

    remoteChange: async (change: RemoteChange) =>
      runMutating(`remote-${change.kind}`, change.name, (path) =>
        invokeFn("cmd_remote_change", { repoPath: path, change }),
      ),

    listSubmodules: async (): Promise<{ submodules: SubmoduleInfo[]; truncated: boolean }> => {
      const session = activeSession();
      if (!session) return { submodules: [], truncated: false };
      const raw = await invokeFn<unknown>("cmd_list_submodules", { repoPath: session.path });
      const parsed = parseSubmoduleList(raw);
      if (parsed.failed) {
        throw new Error("The submodule list was not readable.");
      }
      return { submodules: parsed.submodules, truncated: parsed.truncated };
    },

    submoduleChange: async (change: SubmoduleChange) =>
      runMutating(
        `submodule-${change.kind}`,
        ("path" in change && change.path) || "all submodules",
        (path) => invokeFn("cmd_submodule_change", { repoPath: path, change }),
      ),

    createTag: async (tagName: string, commitId?: string, message?: string) =>
      runMutating("tag", tagName, (path) =>
        invokeFn("cmd_create_tag", {
          repoPath: path,
          tagName,
          commitId: commitId ?? null,
          message: message ?? null,
        }),
      ),

    deleteTag: async (tagName: string) =>
      runMutating("tag-delete", tagName, (path) =>
        invokeFn("cmd_delete_tag", { repoPath: path, tagName }),
      ),

    // --- workspace-wide -------------------------------------------------

    /**
     * The work-in-progress answer for every open repository.
     *
     * Derived from live session state rather than refetched, so it is free to
     * call and always agrees with what the tabs are showing.
     */
    workspaceWip: (): WorkspaceWip => summarizeWorkspace(wipInputs()),

    /**
     * Live facts for every open repository, in tab order.
     *
     * Derived from session state rather than refetched, so it is free to call
     * and always agrees with what the tabs are showing. The Fleet grid's
     * cheapest tier is exactly this call.
     */
    repoFacts: (): RepoFacts[] => repoFacts(),

    /**
     * Runs `fetch` (or `pull`) across every open repository.
     *
     * Repositories that are parked mid-operation, still loading, or holding
     * conflicts are SKIPPED and reported as skipped — never silently counted as
     * fetched. After the sweep every visited repository is refreshed so the
     * tabs reflect what actually landed.
     */
    runAcrossOpenRepos: async (
      kind: "fetch" | "pull",
      options: RunOptions & {
        /**
         * Restrict the run to these open repositories.
         *
         * Omitted, every open tab is visited — the workspace-header behaviour.
         * Given, this is still the same run: a single-row fetch from the Fleet
         * grid goes through the identical skip rules, so a repository parked
         * mid-rebase is reported as skipped there too rather than being
         * fetched by a second, laxer code path.
         *
         * A path that is not an open tab is simply absent from the targets; it
         * is never invented into one.
         */
        only?: readonly string[];
      } = {},
    ): Promise<BulkRunReport> => {
      const byPath = new Map(wipInputs().map((input) => [input.path, input]));
      const restrict = options.only ? new Set(options.only) : null;
      const targets: RepoTarget[] = internal.workspace.tabs
        .filter((tab) => restrict === null || restrict.has(tab.path))
        .map((tab) => ({
          path: tab.path,
          label: byPath.get(tab.path)?.label ?? displayName(tab.path),
        }));
      const report = await runAcrossRepos(
        targets,
        async (target) => {
          const input = byPath.get(target.path);
          const skip = input ? bulkSkipReason(input) : "Repository is no longer open.";
          if (skip) return { skip };
          // Both commands are named literally rather than selected into a
          // variable: the IPC contract checker verifies every invoked command
          // against the Rust registry statically, and a computed name is a
          // hole in that check rather than a shortcut.
          if (kind === "fetch") {
            await invokeFn("cmd_fetch", { repoPath: target.path });
          } else {
            await invokeFn("cmd_pull", { repoPath: target.path });
          }
          mutationEchoUntil.set(target.path, Date.now() + WATCHER_ECHO_SUPPRESS_MS);
        },
        options,
      );
      // Refresh only what actually ran: re-hydrating a skipped repository would
      // cost a full snapshot for a repository nothing happened to.
      //
      // Bounded, not `Promise.all`. Each refresh is itself five concurrent
      // commands, so fanning out over all 64 possible repositories issued
      // ~320 git-spawning calls in one instant and exhausted the process's
      // file descriptors — every later spawn failing with "Too many open
      // files" until restart.
      await mapItems(
        report.results.filter((result) => result.status === "ok"),
        DEFAULT_FAN_OUT,
        (result) => store.refresh(result.path),
      );
      return report;
    },

    /**
     * Aborts, continues, or skips the parked operation.
     *
     * The kind is deliberately NOT sent: the backend re-detects it under the
     * repository lock, so a banner rendered before someone aborted from the
     * terminal cannot send `git rebase --abort` at a repository that has since
     * become idle. The refresh that `runMutating` triggers is what clears the
     * banner, so the UI never has to guess whether the operation ended.
     */
    operationAction: async (action: OperationAction) =>
      runMutating(`operation-${action}`, action, (path) =>
        invokeFn("cmd_repo_operation_action", { repoPath: path, action }),
      ),

    rebaseInteractive: (ontoCommit: string, steps: RebaseStep[]) =>
      runMutating("rebase", ontoCommit, (path) =>
        invokeFn("cmd_rebase_interactive", { repoPath: path, ontoCommit, steps })),

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
    /**
     * File a GitHub issue on a checkout's remote. Without `target` that is the
     * active repository; a task board names its task's own checkout, which
     * need not be open at all. Either way the verdict and the action land in
     * the same harness record. Nothing in `.git` changes, so the explicit
     * form neither refreshes a session nor files its error on one — the
     * caller that asked owns the failure.
     */
    reportIssue: async (
      title: string,
      body: string,
      labels: string[] = [],
      target?: { repoPath: string },
    ): Promise<MutationOutcome<string>> => {
      const create = (path: string) =>
        invokeFn("cmd_github_create_issue", { repoPath: path, title, body, labels });
      const label = title.slice(0, 80);
      if (!target) return runMutating<string>("issue-report", label, create);
      const path = target.repoPath.trim();
      if (!path) return { ok: false, error: "No repository checkout was given for the issue." };
      try {
        const result = await create(path);
        const policy = recordPolicyVerdict(result, path);
        harnessStore.recordAction({ repoPath: path, kind: "issue-report", label, ok: true, verdict: policy ?? null });
        const output = mutationOutput<string>(result);
        return output === undefined ? { ok: true, policy } : { ok: true, policy, output };
      } catch (err: unknown) {
        harnessStore.recordAction({ repoPath: path, kind: "issue-report", label, ok: false });
        return { ok: false, error: formatError(err) };
      }
    },
    publishRelease: async (tag: string, message: string) =>
      runMutating<ReleasePublishResult>("release-publish", tag, (path) =>
        invokeFn("cmd_publish_release", { repoPath: path, tag, message }),
      ),
    stashSave: async (message?: string, options?: StashSaveOptions) =>
      runMutating("stash", message ?? "", (path) =>
        invokeFn("cmd_stash_save", { repoPath: path, message, options }),
      ),
    stashPop: async () => {
      // The menu/palette "Pop" used to call `git stash pop` on stash@{0}
      // sight-unseen. The stash stack is shared with every worktree and
      // agent: that path is what silently applied (or dropped) someone
      // else's entry. Pop the entry this session last listed, by object
      // id; refuse if the list could not be read or is empty.
      const session = activeSession();
      if (!session) return { ok: false as const, error: "No repository is open." };
      if (session.stashFailed) {
        return {
          ok: false as const,
          error:
            "The stash list could not be read, so popping it would target the wrong entry.",
        };
      }
      const top = session.stashEntries[0];
      if (!top) return { ok: false as const, error: "Nothing is stashed." };
      return runMutating("unstash", top.selector, (path) =>
        invokeFn("cmd_stash_action", {
          repoPath: path,
          action: "pop",
          index: top.index,
          expectedOid: top.oid,
        }),
      );
    },
    /**
     * Switch views, optionally landing on a particular section.
     *
     * Omitting `section` keeps whichever lens that view was last left on —
     * the point of remembering it per view. Passing one is how a caller says
     * "show them *this*", which is what the retired top-level tabs did by
     * being separate destinations.
     */
    setActiveTab: (tab: ViewTab, section?: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session) return;
      const resolved = section ? resolveSection(tab, section) : null;
      applyToSession(session.id, session.generation, {
        activeTab: tab,
        ...(resolved
          ? { viewSections: { ...session.viewSections, [tab]: resolved } }
          : {}),
      });
      // The view tab is persisted state: flush so quitting right after a
      // switch does not restore the previous one.
      flushPersist();
    },
    /** Change the lens within a view without leaving it. */
    setViewSection: (tab: ViewTab, section: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session) return;
      const resolved = resolveSection(tab, section);
      if (!resolved || session.viewSections[tab] === resolved) return;
      applyToSession(session.id, session.generation, {
        viewSections: { ...session.viewSections, [tab]: resolved },
      });
      flushPersist();
    },
    /**
     * Shows or hides the terminal dock on the ACTIVE repository tab.
     *
     * Scoped to one tab on purpose: a shell belongs to a working tree, and a
     * workspace-wide flag made every repository the user switched to inherit
     * — and start — a terminal they never opened.
     *
     * Hiding does not end the shells. `TerminalDock` keeps a hidden panel
     * mounted so its scrollback survives; closing the repository tab is what
     * disposes it. Returns whether anything changed, so a caller that opens
     * the dock to reveal something can tell a no-op from a real open.
     */
    setTerminalOpen: (open: boolean): boolean => {
      const session = activeSession();
      if (!session || session.terminalOpen === open) return false;
      if (!applyToSession(session.id, session.generation, { terminalOpen: open })) return false;
      flushPersist();
      return true;
    },
    toggleTerminal: (): boolean => {
      const session = activeSession();
      if (!session) return false;
      if (!applyToSession(session.id, session.generation, { terminalOpen: !session.terminalOpen })) return false;
      flushPersist();
      return true;
    },
    inspectCommitInHistory: (commitId: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session || !commitId) return;
      applyToSession(session.id, session.generation, {
        selectedCommitId: commitId,
        selectedFilePath: null,
        selectedDiff: null,
        selectedDiffTruncated: false,
        selectedDiffTruncationReason: null,
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
  /**
   * Per-repository live facts, one record per open tab.
   *
   * `unpushedCommits` comes from the current branch's ahead count; a session
   * that has not hydrated reports zero, and `hydrated: false` is what tells
   * every consumer to treat that zero as unknown rather than as "nothing to
   * push". The same rule governs every other count here.
   *
   * This is the single extraction from session state. The work-in-progress
   * summary and the Fleet grid both narrow from it (see repos/facts.ts)
   * instead of walking `internal.sessions` themselves.
   */
  function repoFacts(): RepoFacts[] {
    const labels = disambiguateLabels(internal.workspace.tabs.map((tab) => tab.path));
    return internal.workspace.tabs.map((tab) => {
      const session = internal.sessions[tab.id];
      const statuses = session?.statuses ?? [];
      const current = session?.branches.find(
        (branch) => branch.is_current || branch.name === session?.currentBranch,
      );
      let additions = 0;
      let deletions = 0;
      let churnPartial = false;
      for (const file of statuses) {
        additions += file.additions;
        deletions += file.deletions;
        // Rust omits the key entirely while empty, so this is absent on the
        // overwhelming majority of rows. Where it is present, the sums above
        // are floors and must be rendered as such.
        if (file.warnings && file.warnings.length > 0) churnPartial = true;
      }
      return {
        path: tab.path,
        label: labels.get(tab.path) ?? displayName(tab.path),
        branch: session?.currentBranch ?? null,
        isBare: Boolean(session?.isBare),
        changedFiles: statuses.length,
        conflictedFiles: statuses.filter((file) => file.is_conflicted).length,
        stagedFiles: statuses.filter((file) => file.is_staged).length,
        additions,
        deletions,
        churnPartial,
        unpushedCommits: current?.ahead_count ?? 0,
        behindCommits: current?.behind_count ?? 0,
        // An unreadable stash list must not read as an empty one; consumers
        // treat a failed probe as unknown through `stashFailed`/`loadFailed`.
        stashEntries: session?.stashEntries.length ?? 0,
        stashFailed: Boolean(session?.stashFailed || session?.stashTruncated),
        operation: session?.operation ?? IDLE_OPERATION,
        watch: session?.watch ?? WATCH_UNKNOWN,
        loadFailed: Boolean(session?.error),
        loadError: session?.error ?? null,
        hydrated: Boolean(session?.hasHydrated),
      };
    });
  }

  /** The work-in-progress model's narrower view of the same facts. */
  function wipInputs(): RepoWipInput[] {
    return repoFacts().map(toWipInput);
  }

  async function runStageBatch(kind: IndexAction): Promise<MutationOutcome> {
    const session = activeSession();
    if (!session) return { ok: false, error: "No active repository" };
    const files = session.statuses.filter((file) => kind === "unstage" ? file.is_staged : hasUnstagedChanges(file));
    if (files.length === 0) return { ok: true };
    if (kind === "stage" && files.some((file) => file.is_conflicted)) {
      return { ok: false, error: "Resolve conflicts before staging all files. Stage each resolved file explicitly." };
    }
    const filePaths = indexSelectionPaths(files, kind);
    const finishActivity = beginMutation(session.path, `${kind}-all`);
    try {
      const outcome = await runMutating(`${kind}-all`, `${filePaths.length} paths`, (path) =>
        invokeFn("cmd_change_index", { repoPath: path, filePaths, action: kind }), { session, trackActivity: false });
      // Native batches can stop after a completed chunk. Keep activity alive
      // through recovery and retain the diagnostic after hydration.
      if (!outcome.ok) {
        await store.refresh(session.path);
        applyToSession(session.id, session.generation, { error: outcome.error ?? "Index update failed" });
      }
      return outcome;
    } finally {
      finishActivity();
    }
  }

  async function runMutating<T = unknown>(
    kind: string,
    label: string,
    action: (path: string) => Promise<unknown>,
    opts: { skipRefresh?: boolean; session?: RepoSession; trackActivity?: boolean } = {},
  ): Promise<MutationOutcome<T>> {
    const session = opts.session ?? activeSession();
    if (!session) return { ok: false, error: "No repository is open." };
    const path = session.path;
    const generation = session.generation;
    const finishActivity = opts.trackActivity === false ? () => {} : beginMutation(path, kind);
    try {
      const result = await action(path);
      // Arm the echo window BEFORE anything downstream observes the change:
      // the mutation's own `.git` writes are about to bounce back as watcher
      // events (see WATCHER_ECHO_SUPPRESS_MS).
      mutationEchoUntil.set(path, Date.now() + WATCHER_ECHO_SUPPRESS_MS);
      const policy = recordPolicyVerdict(result, path);
      harnessStore.recordAction({
        repoPath: path,
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
          internal.workspace.activeId === session.id &&
          REFETCH_SELECTION_KINDS.has(kind) &&
          still.selectionKind === "file" &&
          still.selectedFilePath &&
          !still.selectedCommitId &&
          isSectionOnScreen(still.activeTab, still.viewSections, "history", "diff")
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
      harnessStore.recordAction({ repoPath: path, kind, label, ok: false });
      return { ok: false, error: formatError(err) };
    } finally {
      finishActivity();
    }
  }

  return store;
}

/**
 * Files a `Guarded<T>` result's verdict with the harness store, and clears the
 * last verdict for an action that carried none.
 */
function recordPolicyVerdict(
  result: unknown,
  repoPath: string,
): PolicyVerdict | undefined {
  if (result && typeof result === "object" && "policy" in result) {
    const policy = (result as { policy: PolicyVerdict }).policy;
    if (policy && typeof policy.status === "string") {
      harnessStore.recordVerdict(policy, repoPath);
      return policy;
    }
  }
  harnessStore.recordVerdict(null, repoPath);
  return undefined;
}

function mutationOutput<T>(result: unknown): T | undefined {
  if (result && typeof result === "object" && "output" in result) {
    return (result as { output: T }).output;
  }
  return result === undefined ? undefined : (result as T);
}

export const repoStore = createRepoStore();
