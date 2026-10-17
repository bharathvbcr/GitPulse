/**
 * Reading `cmd_worktree_merge_teardown`'s typed failure.
 *
 * The host rejects with `{code, message, ...}` (`engine::worktree::
 * MergeTeardownError`), where every other command rejects with a string. A
 * rejection that is not exactly that shape is not trusted to be one: the
 * caller keeps the original and reports it as it always did.
 */

import type { MergeTeardownError } from "./types";

/** A merge-and-teardown that did not merge, carrying the host's typed reason. */
export class MergeTeardownFailure extends Error {
  readonly failure: MergeTeardownError;

  constructor(failure: MergeTeardownError) {
    super(failure.message);
    this.name = "MergeTeardownFailure";
    this.failure = failure;
  }
}

const isText = (value: unknown): value is string => typeof value === "string";
const isCount = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0;

/** The typed failure in `cause`, or null when it is anything else. */
export function readMergeTeardownError(cause: unknown): MergeTeardownError | null {
  if (cause instanceof MergeTeardownFailure) return cause.failure;
  if (cause === null || typeof cause !== "object") return null;
  const v = cause as Record<string, unknown>;
  const message = v.message;
  if (!isText(message) || message.trim() === "") return null;
  switch (v.code) {
    case "conflict":
      if (Array.isArray(v.paths) && v.paths.every(isText) && isCount(v.total) && typeof v.rolled_back === "boolean" && (v.rollback_error === null || isText(v.rollback_error))) {
        return { code: "conflict", message, paths: v.paths, total: v.total, rolled_back: v.rolled_back, rollback_error: v.rollback_error };
      }
      return null;
    case "moved":
      return isText(v.branch) && isText(v.expected) && isText(v.actual)
        ? { code: "moved", message, branch: v.branch, expected: v.expected, actual: v.actual }
        : null;
    case "base_mismatch":
      return isText(v.base_branch) && isText(v.target_branch) && isText(v.base_oid)
        ? { code: "base_mismatch", message, base_branch: v.base_branch, target_branch: v.target_branch, base_oid: v.base_oid }
        : null;
    case "squash_parked":
      return { code: "squash_parked", message };
    case "refused":
      return { code: "refused", message };
    default:
      return null;
  }
}
