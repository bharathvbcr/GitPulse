/**
 * The live facts one open repository publishes about itself.
 *
 * `wipSummary` answers a narrow question — is work at risk here — and its
 * {@link RepoWipInput} carries exactly the fields that answer it. The Fleet
 * dashboard asks a wider one, and asking it used to mean either a second,
 * near-identical extraction inside the store or a `RepoWipInput` quietly
 * growing fields that the risk model does not read.
 *
 * So the store produces `RepoFacts` once, and `RepoWipInput` becomes a
 * projection of it ({@link toWipInput}). One canonical owner of "what is true
 * about this repository right now"; every consumer narrows from it rather
 * than re-deriving it from session internals.
 *
 * Everything here is derived from state already in memory. No IPC, no caching,
 * no async — which is what makes the Fleet grid's cheapest tier free.
 */

import { IDLE_OPERATION, type OperationState } from "./operation";
import { disambiguateLabels, displayName } from "./paths";
import type { RepoSession } from "./storeTypes";
import { WATCH_UNKNOWN, type WatchState } from "./watchState";
import type { RepoWipInput } from "./wipSummary";

export interface RepoFacts {
  /** Absolute repository path — the identity every backend command takes. */
  path: string;
  /** Disambiguated display label; falls back to the directory name. */
  label: string;
  /** Checked-out branch, or null when detached / not yet known. */
  branch: string | null;
  isBare: boolean;

  /** Files with working-tree or index changes. */
  changedFiles: number;
  /** Of those, how many carry conflict markers. */
  conflictedFiles: number;
  /** Of those, how many are staged. */
  stagedFiles: number;
  /** Line churn across the working tree, summed from the status rows. */
  additions: number;
  deletions: number;
  /**
   * True when at least one status row admitted its numstat was unparseable,
   * so `additions`/`deletions` are floors rather than totals.
   */
  churnPartial: boolean;

  /** Commits on the current branch not present upstream. */
  unpushedCommits: number;
  /** Commits upstream has that the current branch does not. */
  behindCommits: number;

  /** Entries on the stash stack. */
  stashEntries: number;
  /** True when the stash probe itself failed, so 0 is not "none". */
  stashFailed: boolean;

  /** The parked operation facet, including whether its probe ran. */
  operation: OperationState;
  /** Whether this repository is receiving live filesystem updates. */
  watch: WatchState;

  /** True when this repository's snapshot failed to load at all. */
  loadFailed: boolean;
  /** The load failure's text, when there was one. */
  loadError: string | null;
  /** True before the first snapshot has landed; nothing above is knowable yet. */
  hydrated: boolean;
}

/** A `RepoFacts` for a path with no session at all — every fact unknown. */
export function unknownFacts(path: string, label: string): RepoFacts {
  return {
    path,
    label,
    branch: null,
    isBare: false,
    changedFiles: 0,
    conflictedFiles: 0,
    stagedFiles: 0,
    additions: 0,
    deletions: 0,
    churnPartial: false,
    unpushedCommits: 0,
    behindCommits: 0,
    stashEntries: 0,
    stashFailed: false,
    operation: IDLE_OPERATION,
    watch: WATCH_UNKNOWN,
    loadFailed: false,
    loadError: null,
    // The load-bearing field: `hydrated: false` is what stops every zero above
    // from being read as a measurement.
    hydrated: false,
  };
}

/**
 * Narrows the full fact set to what the work-in-progress model reads.
 *
 * `loadFailed` folds in a failed stash probe deliberately: the risk model has
 * one "we could not read this repository" channel, and an unreadable stash is
 * exactly that — work that exists nowhere else, invisible.
 */
export function toWipInput(facts: RepoFacts): RepoWipInput {
  return {
    path: facts.path,
    label: facts.label,
    changedFiles: facts.changedFiles,
    conflictedFiles: facts.conflictedFiles,
    unpushedCommits: facts.unpushedCommits,
    stashEntries: facts.stashEntries,
    operation: facts.operation,
    loadFailed: facts.loadFailed || facts.stashFailed,
    hydrated: facts.hydrated,
  };
}

/**
 * Open paths that may receive background work. A session with
 * `trustRequired`, or one whose checkout is `missing`, is omitted, and it is
 * not used as the active key. A missing checkout used to stay in: every
 * workspace sync re-tried it and re-reported it, once per activation.
 */
export function pathsTrustedForBackground(
  tabs: readonly { path: string; trustRequired?: boolean; missing?: boolean; parked?: boolean }[],
  activePath: string | null,
): { activeKey: string | null; retainedKeys: string[]; liveKeys: string[] } {
  const retainedKeys: string[] = [];
  const liveKeys: string[] = [];
  let activeBlocked = false;
  for (const tab of tabs) {
    if (tab.trustRequired || tab.missing) {
      if (tab.path === activePath) activeBlocked = true;
      continue;
    }
    retainedKeys.push(tab.path);
    // Parked: open but not watched (repos/watchPool.ts). The active tab is
    // never parked, so this cannot drop the repository being looked at.
    if (!tab.parked || tab.path === activePath) liveKeys.push(tab.path);
  }
  return {
    activeKey: activePath && !activeBlocked ? activePath : null,
    retainedKeys,
    liveKeys,
  };
}

/**
 * Per-repository live facts, one record per open tab.
 *
 * `unpushedCommits` comes from the current branch's ahead count; a session
 * that has not hydrated reports zero, and `hydrated: false` is what tells
 * every consumer to treat that zero as unknown rather than as "nothing to
 * push". The same rule governs every other count here.
 *
 * This is the single extraction from session state. The work-in-progress
 * summary and the Fleet grid both narrow from it instead of walking
 * `internal.sessions` themselves.
 */
export function collectRepoFacts(
  tabs: readonly { id: string; path: string }[],
  sessions: Readonly<Record<string, RepoSession | undefined>>,
): RepoFacts[] {
  const labels = disambiguateLabels(tabs.map((tab) => tab.path));
  return tabs.map((tab) => {
    const session = sessions[tab.id];
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
