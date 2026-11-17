import { humanBytes } from "./format";
import type { StorageReport, WorktreeUsage } from "./types";

/**
 * One linked worktree's size as the Storage panel states it. The logical size
 * of a worktree whose `target/` was cloned from the main checkout is tens of
 * gigabytes that removing it does not free, so the freed figure leads when it
 * was read. A size cut short by the walk's budget says so and how far it got.
 */
export function worktreeSizeText(w: WorktreeUsage): string {
  const floor = w.truncated ? "≥ " : "";
  const logical = `${floor}${humanBytes(w.bytes)} logical`;
  const counted = w.truncated ? ` · first ${w.files_counted.toLocaleString()} files` : "";
  if (w.private_bytes === undefined) return `${logical}${counted}`;
  return `${floor}${humanBytes(w.private_bytes)} freed by removal · ${logical}${counted}`;
}

/** The section heading's count: sized of listed, never the sized list as if it were all. */
export function worktreeCountText(report: Pick<StorageReport, "worktrees" | "worktrees_listed">): string {
  const sized = report.worktrees.length;
  const listed = Math.max(report.worktrees_listed ?? sized, sized);
  return listed > sized ? `${sized} sized of ${listed}` : `${sized}`;
}
