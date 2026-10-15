import { type PolicyVerdict } from "../stores/harnessStore";
import { type BranchInfo, type TagInfo } from "../branches/types";
import { type FilterState } from "../stores/filterStore";
import type { InvokeFn } from "../stores/graphStore";
import { type WorkspaceTabs } from "./tabModel";
import { type StorageLike, type ViewTab } from "./persist";
import { type GroupColor, type TabColor } from "./tabColors";
import { type StashEntry } from "./stash";
import { type WatchState, type WatchStatus } from "./watchState";
import { type OperationState } from "./operation";

/** What a mutating action reports back: whether it ran, and under what verdict. */
export interface MutationOutcome<T = unknown> {
  ok: boolean;
  error?: string;
  policy?: PolicyVerdict;
  output?: T;
  /**
   * On a failure: whether the error was filed on the session that is active
   * now, so the app's error toast already shows it. A refusal, or a failure in
   * a background tab, is not surfaced; the caller says it with `reportFailure`.
   */
  surfaced?: boolean;
  /** The target no longer names an open, trusted, present repository; nothing ran. */
  refused?: boolean;
}

/**
 * The repository a mutation was asked about, captured before the prompt, the
 * confirm or the toast that precedes it. A tab id always names the same
 * repository, so identity is tab id plus path, never a generation: activation
 * bumps the generation, which would refuse every Undo made after a tab switch.
 */
export interface MutationTarget {
  readonly tabId: string;
  readonly path: string;
}

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
  /** The checkout was deleted or moved while open; see `RepoSession.missing`. */
  missing?: boolean;
  currentBranch: string | null;
  conflictedCount: number;
  changedCount: number;
  /**
   * Whether the repository is watched (see repos/watchPool.ts): `parked`
   * means more repositories are open than are watched at once, and this one
   * catches up when it is opened. Absent in fixtures that predate the pool.
   */
  watch?: WatchStatus;
  /**
   * False until the repository has been read once, so the counts above are
   * not a clean bill of health for a parked tab restore never read.
   */
  countsKnown?: boolean;
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
  /**
   * Why the selected diff could not be read, or null. Per selection and kept
   * apart from `error` (the repository snapshot's): a failed diff read must
   * not blank the status, disable Review changes, or leave the previous
   * file's diff on screen under the new file's name.
   */
  selectedDiffError: string | null;
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
  isAmending: boolean;
  isLoading: boolean;
  error: string | null;
  /**
   * True when the last open or hydrate of this session failed with
   * `REPOSITORY_TRUST_REQUIRED`. A later successful hydrate clears it.
   * Schedulers treat it as "do not call the backend for this path".
   */
  trustRequired: boolean;
  /**
   * The watcher saw this checkout's git directory disappear (`repo-gone`).
   * Background work skips it, as it does `trustRequired`; a later successful
   * hydrate (the checkout was recreated) clears it.
   */
  missing: boolean;
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
  /**
   * Why the selected diff could not be read, or null. Per selection and kept
   * apart from `error` (the repository snapshot's): a failed diff read must
   * not blank the status, disable Review changes, or leave the previous
   * file's diff on screen under the new file's name.
   */
  selectedDiffError: string | null;
  /** What the selection is: a worktree file, a commit (optionally one of its files), or a range. */
  selectionKind: SelectionKind;
  activeTab: ViewTab;
  /** Section last open in each sectioned view; see the session field. */
  viewSections: Record<string, string>;
  /** Whether the ACTIVE tab is showing the terminal dock; see the session field. */
  terminalOpen: boolean;
  isLoading: boolean;
  error: string | null;
  /** Active session needs trust before Git or background work may run. */
  trustRequired: boolean;
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

export interface InternalState {
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
    /**
     * Of `countFor`, those running in another checkout of the repository —
     * agents this tab hosts (`taskLaunches.hostTabFor`). Closing the tab ends
     * them too, which the question has to say: they are not in it.
     */
    hostedFor?(repoPath: string): number;
  };
}
