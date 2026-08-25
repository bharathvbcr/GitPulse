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
 * Panels extend this with their own sections (PRs, issues, …).
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
}
