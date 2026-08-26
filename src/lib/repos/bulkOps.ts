import type { MutationOutcome } from "../stores/repoStore";

/**
 * Aggregates per-file MutationOutcomes from a bulk action (stage all /
 * unstage all) into ONE honest outcome. The pre-fix loop discarded every
 * per-file result and returned `{ ok: true }` even when most files failed —
 * a total failure was indistinguishable from success.
 *
 * - Zero inputs: `{ ok: true }` (nothing to do is success).
 * - All succeeded: `{ ok: true }`.
 * - Any failure: `{ ok: false, error }` where the message names how many of
 *   N files staged/unstaged plus the FIRST failure's reason; later failures
 *   are still executed by the caller (each file is independent), they just
 *   don't bury the first diagnostic.
 */
export function summarizeBulkOutcome(
  outcomes: readonly MutationOutcome[],
  verb: string,
): MutationOutcome {
  const total = outcomes.length;
  if (total === 0) return { ok: true };
  const failed = outcomes.filter((o) => !o.ok);
  if (failed.length === 0) return { ok: true };
  const firstError = failed[0].error ?? "unknown error";
  const detail =
    failed.length === 1
      ? firstError
      : `${firstError} (+${failed.length - 1} more ${verb} failure${failed.length - 1 > 1 ? "s" : ""})`;
  return {
    ok: false,
    error: `${verb} ${total - failed.length} of ${total}: ${detail}`,
  };
}
