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

/**
 * Fields of the `cmd_github_context` payload that more than one panel reads.
 * Panels extend this with their own sections (PRs, issues, …).
 */
export interface GitHubContextBase {
  available: boolean;
  owner: string;
  repo: string;
  workflow_runs: WorkflowRunInfo[];
  error?: string | null;
}
