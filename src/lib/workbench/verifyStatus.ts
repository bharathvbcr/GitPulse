/**
 * Posting a task's verification verdict to GitHub as a commit status
 * (`devcouncil/verify`) and, when asked, a pull-request comment, through the
 * user's own `gh` login.
 *
 * Off for a repository until the person turns it on. Only a person's action
 * reaches `postVerifyResult`; no MCP tool does. A verdict that could not be
 * established posts `error`, never `success`, and one that cannot be tied to
 * the commit posts nothing (the call rejects with the reason).
 */
import { invoke } from "../ipc/invoke";

export interface PostedVerifyStatus {
  sha: string;
  state: "success" | "failure" | "error";
  description: string;
  dry_run: boolean;
  status_posted: boolean;
  /** Pull requests a comment landed on. */
  commented: number[];
  /** Why a requested comment was not posted, when it was not. */
  comment_note: string | null;
  /** Dry run only: the `gh` argv lists that would run. Nothing was sent. */
  would_run: string[][];
}

/** Rejects when the setting cannot be read, never resolves `true` on a guess. */
export function verifyStatusEnabled(repoPath: string): Promise<boolean> {
  return invoke<boolean>("cmd_verify_status_enabled", { repoPath });
}

export function setVerifyStatus(repoPath: string, enabled: boolean): Promise<void> {
  return invoke<void>("cmd_verify_status_save", { repoPath, enabled });
}

export function postVerifyResult(
  repoPath: string,
  taskId: string,
  options: { comment?: boolean; dryRun?: boolean } = {},
): Promise<PostedVerifyStatus> {
  return invoke<PostedVerifyStatus>("cmd_verify_status_post", {
    repoPath,
    taskId,
    comment: options.comment === true,
    dryRun: options.dryRun === true,
  });
}
