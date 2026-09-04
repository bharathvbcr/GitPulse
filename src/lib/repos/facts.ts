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
