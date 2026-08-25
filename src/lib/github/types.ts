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
