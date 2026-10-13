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

/**
 * Whether the task sheet may offer "Post verify result". Only an opt-in the
 * app actually read as `true` enables it: not yet read (`null`), unreadable and
 * off all keep it disabled, and each says why.
 */
export function postAvailability(
  enabled: boolean | null,
  readError: string,
  repoPath: string,
): { allowed: boolean; reason: string } {
  if (repoPath.trim() === "") return { allowed: false, reason: "Open a repository to post its verdict." };
  if (readError) return { allowed: false, reason: `The opt-in setting could not be read, so nothing can be posted: ${readError}` };
  if (enabled === null) return { allowed: false, reason: "Checking whether this repository posts verification results…" };
  if (!enabled) return { allowed: false, reason: "Posting verification results to GitHub is off for this repository." };
  return { allowed: true, reason: "" };
}

/** The confirmation a person answers before anything is sent to GitHub. */
export const POST_CONFIRM = Object.freeze({
  title: "Post verify result?",
  message:
    "This posts the task's latest verdict to GitHub as the devcouncil/verify commit status on the verified commit, through your own gh login. Anyone who can see the repository's commit statuses will see it. A verdict that is out of date or could not be established is posted as an error, never as a pass.",
  confirmLabel: "Post to GitHub",
  cancelLabel: "Cancel",
});

/** One line saying what a post did, for the sheet. */
export function postedSummary(posted: PostedVerifyStatus): string {
  const sha = posted.sha.slice(0, 7);
  const comments = posted.commented.length > 0 ? ` and commented on #${posted.commented.join(", #")}` : "";
  return posted.status_posted ? `Posted ${posted.state} for ${sha}${comments}.` : `Nothing was posted for ${sha}.`;
}
