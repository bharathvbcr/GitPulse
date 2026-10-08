/**
 * The review an agent attempt's commits need before they are merged, when the
 * repository has the review gate on (`docs/AGENT_OUTPUT_REVIEW.md`).
 *
 * The record lives in the workbench store beside the run; the host creates
 * and decides it in one step on the person's action, so these calls name only
 * the run and the decision. The gate that consults it runs on every guarded
 * merge or cherry-pick, so the attempt's Merge button meets it like any other.
 */
import { invoke } from "../ipc/invoke";

export type ReviewDecision = "approve" | "request_changes" | "deny" | "merge_unreviewed";

export type ReviewStatus =
  | { status: "approved"; note: string | null }
  | { status: "overridden"; note: string }
  | { status: "changes_requested"; note: string | null }
  | { status: "denied"; note: string | null }
  | { status: "stale"; reviewed_head: string }
  | { status: "unreviewed" };

export interface AttemptReview {
  run_id: string;
  branch: string;
  base_oid: string;
  head_oid: string;
  files_changed: number;
  ended: boolean;
  review: ReviewStatus;
  /** `null` when the setting could not be read; `gate_error` says why. */
  gate_on: boolean | null;
  gate_error: string | null;
}

export function readAttemptReview(runId: string): Promise<AttemptReview> {
  return invoke<AttemptReview>("cmd_attempt_review", { runId });
}

/** `merge_unreviewed` needs a note: the store refuses an override without one. */
export function recordAttemptReview(runId: string, decision: ReviewDecision, note: string): Promise<AttemptReview> {
  const trimmed = note.trim();
  return invoke<AttemptReview>("cmd_attempt_review_record", { runId, decision, note: trimmed ? trimmed : null });
}

export function setReviewGate(repoPath: string, enabled: boolean): Promise<void> {
  return invoke<void>("cmd_review_gate_save", { repoPath, enabled });
}

const short = (oid: string) => oid.slice(0, 12);

/** One sentence for where the review stands, for the attempt's row. */
export function reviewSummary(view: AttemptReview): string {
  const range = `${view.branch} @ ${short(view.head_oid)} (${view.files_changed} file${view.files_changed === 1 ? "" : "s"} since ${short(view.base_oid)})`;
  const note = (text: string | null) => (text ? ` Note: ${text}` : "");
  switch (view.review.status) {
    case "approved": return `Approved for merge: ${range}.${note(view.review.note)}`;
    case "overridden": return `Recorded as merged without review: ${range}.${note(view.review.note)}`;
    case "changes_requested": return `Changes requested on ${range}.${note(view.review.note)}`;
    case "denied": return `Denied in review: ${range}.${note(view.review.note)}`;
    case "stale": return `Approved at ${short(view.review.reviewed_head)}, but the branch has moved: ${range} needs a new review.`;
    case "unreviewed": return view.ended ? `Not reviewed: ${range}.` : `Reviewable once the attempt ends: ${range}.`;
  }
}
