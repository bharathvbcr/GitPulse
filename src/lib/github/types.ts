import type { IssueInfo } from "../ops/model";

/**
 * Wire types for GitHub payloads returned by the Tauri commands. These are
 * the serde field names as serialized by src-tauri; do not rename fields.
 */

export interface WorkflowRunInfo {
  id: number;
  name: string;
  title: string;
  status: string;
  conclusion: string;
  head_branch: string;
  url: string;
  created_at: string;
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
}

export interface ReleaseInfo {
  tag_name: string;
  name: string;
  is_draft: boolean;
  is_prerelease: boolean;
  is_latest: boolean;
  published_at: string;
  created_at: string;
  url: string;
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
}

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
  issues: IssueInfo[];
  /** True when more open issues exist than the display cap kept. */
  issues_truncated: boolean;
  issues_error?: string | null;
}
