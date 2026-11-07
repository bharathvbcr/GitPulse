import type { IssueInfo, ReleaseInfo } from "../ops/model";

export type { ReleaseInfo };

/**
 * Wire types for GitHub payloads returned by the Tauri commands. These are
 * the serde field names as serialized by src-tauri; do not rename fields.
 */

/**
 * What a run's `status` and `conclusion` mean, decided once in Rust
 * (`github/outcome.rs`) and carried on every run. The renderer reads this and
 * keeps no table of its own: two tables are two chances to disagree about
 * whether a run is red.
 *
 * - `needs_person` is `action_required`: a run held for an approval. Nothing
 *   broke, so it is never `failed`, but someone has to act, so the work view
 *   still marks it as needing attention.
 * - `cancelled` is not `failed`, and no red banner is raised for it; the
 *   delivery timeline still counts it as a bad end (see `runLifecycle.ts`).
 * - `unknown` is everything the rule does not name, including a payload that
 *   carried no outcome at all.
 */
export const RUN_OUTCOMES = ["in_flight", "passed", "failed", "cancelled", "needs_person", "unknown"] as const;
export type RunOutcome = (typeof RUN_OUTCOMES)[number];

/** A wire value as an outcome; anything this build does not name is `unknown`, never a verdict. */
export function asRunOutcome(value: unknown): RunOutcome {
  return RUN_OUTCOMES.find((outcome) => outcome === value) ?? "unknown";
}

export interface WorkflowRunInfo {
  /** See {@link RunOutcome}. */
  outcome: RunOutcome;
  id: number;
  name: string;
  title: string;
  status: string;
  conclusion: string;
  head_branch: string;
  url: string;
  created_at: string;
  /**
   * When the run began executing, which is not when it was created — a run can
   * sit queued for minutes. Empty while it has not started, and that emptiness
   * is what separates "queued" from "started at the epoch", so a duration must
   * never fall back to `created_at`.
   */
  started_at: string;
  /** Last change; for a completed run this is when it settled. */
  updated_at: string;
  /** Commit the run was dispatched against, empty when unreported. */
  head_sha: string;
  /** `push` | `pull_request` | `workflow_dispatch` | …, empty when unreported. */
  event: string;
}

/**
 * Wire shape of `cmd_github_runs` — the narrow listing behind the live poll.
 *
 * `checked` is load-bearing: false means the poll could not run, so `runs`
 * being empty says nothing about the repository. Rendering an unchecked report
 * as an empty list is how stale rows get replaced by a confident "no runs".
 */
export interface GitHubRunsReport {
  available: boolean;
  checked: boolean;
  cli_present: boolean;
  owner: string;
  repo: string;
  runs: WorkflowRunInfo[];
  truncated: boolean;
  error: string | null;
  /**
   * Runs that finished red (`outcome: "failed"`) since the previous look at
   * this repository, decided natively by the same observer the CI watcher
   * uses. Empty on the first look and whenever `checked` is false — which is
   * what says whether "none" means none.
   */
  went_red: WorkflowRunInfo[];
}

/** One Actions workflow from `gh workflow list`. */
export interface WorkflowInfo {
  id: number;
  /** Repository-relative workflow file path; the stable dispatch selector. */
  path: string;
  name: string;
  /** `active` | `disabled_manually` | `disabled_inactivity`. */
  state: string;
}

/** Wire shape of `cmd_github_workflows`. */
export interface WorkflowsReport {
  available: boolean;
  cli_present: boolean;
  workflows: WorkflowInfo[];
  truncated: boolean;
  error: string | null;
}

/** One executed (or skipped) step of a local CI run. */
export interface CiStepResult {
  name: string;
  command: string;
  /** `passed` | `failed` | `skipped`. */
  status: string;
  detail: string;
  duration_ms: number;
}

/** Wire shape of `cmd_ci_local`. */
export interface CiLocalReport {
  steps: CiStepResult[];
  passed: number;
  failed: number;
  skipped: number;
  total_duration_ms: number;
  /**
   * The commit this run was recorded against, empty when it was not recorded.
   *
   * A run that produced a verification note is a durable, git-native claim
   * that survives a re-clone; a run that did not is a number on a screen. A
   * sha rather than a boolean so the two are never confused.
   */
  recorded_commit: string;
  /**
   * Empty when the run was recorded; otherwise why it was not.
   *
   * Separate from `recorded_commit` so "recorded nothing because the tree was
   * dirty" never reads the same as "recorded nothing because writing the note
   * failed".
   */
  not_recorded_reason: string;
  /**
   * Empty unless this checkout is an agent attempt's worktree and telling its
   * task about the result failed; then why. Separate from `not_recorded_reason`:
   * the git note and the task's notice are two different records.
   */
  signal_error?: string;
  /**
   * How test steps were scoped. A fail-closed full suite must never read as
   * "affected tests passed".
   */
  test_scope?: CiTestScope;
}

/** How CI:local scoped its test steps for one run. */
export interface CiTestScope {
  /** `affected` when path-filtered; `full_suite` otherwise. */
  mode: string;
  reason: string;
  seeds: string[];
  test_files: string[];
  fail_closed: boolean;
}

/**
 * Fields of the `cmd_github_context` payload that more than one panel reads.
 * Panels extend this with their own sections (PRs, issues, …). The optional
 * degradation channels (`*_error`, `warnings`, `*_truncated`) must be
 * surfaced by consumers: "could not fetch" must never render as a clean,
 * complete-looking empty state.
 */
export interface GitHubContextBase {
  available: boolean;
  owner: string;
  repo: string;
  workflow_runs: WorkflowRunInfo[];
  releases: ReleaseInfo[];
  releases_truncated?: boolean;
  releases_error?: string | null;
  error?: string | null;
  /** Set when the workflow-run listing could not run or be parsed. */
  runs_error?: string | null;
  /** True when more workflow runs exist than the display cap kept. */
  runs_truncated?: boolean;
  /**
   * Section-level degradations that did not fail the whole context (e.g. a
   * PR-listing parse failure). Empty while everything fetched cleanly.
   */
  warnings?: string[];
}

/** Mirrors `PullRequestInfo` in src-tauri/src/github/mod.rs field for field. */
export interface PullRequestInfo {
  number: number;
  title: string;
  state: string;
  head_ref: string;
  base_ref: string;
  url: string;
  is_draft: boolean;
  ci_status: string;
  created_at: string;
  updated_at: string;
  review_decision: string;
  /** Empty when nobody has reviewed yet — not a zero-hour review. */
  first_review_at: string;
  /** True when the head branch lives in a fork: `head_ref` then names a
   *  branch of the contributor's repository, not of this one. */
  is_cross_repository?: boolean;
  /** Login of the head repository's owner; empty when gh did not report it. */
  head_owner?: string;
}

/** Mirrors `PullRequestDetail` in src-tauri/src/github/mod.rs (`gh pr view`). */
export interface PullRequestDetail {
  number: number;
  title: string;
  state: string;
  url: string;
  is_draft: boolean;
  author: string;
  head_ref: string;
  base_ref: string;
  /** The head a merge is pinned to with `--match-head-commit`. */
  head_oid: string;
  body: string;
  body_truncated: boolean;
  additions: number;
  deletions: number;
  changed_files: number;
  mergeable: string;
  merge_state: string;
  review_decision: string;
  ci_status: string;
}

export type PrReviewVerdict = "approve" | "request_changes" | "comment";
export type PrMergeMethod = "merge" | "squash" | "rebase";

/** Mirrors the `PrAction` enum `cmd_github_pr_action` takes. */
export type PrAction =
  | { kind: "create"; title: string; body: string; base: string; draft: boolean }
  | { kind: "review"; number: number; verdict: PrReviewVerdict; body: string }
  | { kind: "merge"; number: number; method: PrMergeMethod; delete_branch: boolean; head_oid: string };

/**
 * The full context payload. Declared inside GitHubPanel until `check:types`
 * grew `extends` resolution and could read it here, next to the base whose
 * fields it inherits.
 */
export interface GitHubContext extends GitHubContextBase {
  cli_present: boolean;
  host: string;
  html_url: string;
  pull_requests: PullRequestInfo[];
  /** True when more open PRs exist than the display cap kept. */
  prs_truncated?: boolean;
  /** Set when the pull-request listing could not run or be parsed. */
  prs_error?: string | null;
  issues: IssueInfo[];
  /** True when more open issues exist than the display cap kept. */
  issues_truncated: boolean;
  issues_error?: string | null;
}
