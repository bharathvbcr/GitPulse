import type { LiveIndexPhase } from "./liveIndex";

/**
 * Why a manual Build or Refresh would be pointless right now, or `null` when
 * pressing it is fine. One owner so the strip's two buttons and the empty
 * state's button cannot disagree.
 *
 * A missing CLI wins over a running index: the CLI is the cause the user can
 * act on. `failed`, `paused` and `skipped` are deliberately not blockers — the
 * automatic path has stopped, so a manual build is the user's recourse.
 */
export const CLI_MISSING_BUILD_REASON = "devmap CLI is not installed — run Setup first";
export const INDEX_RUNNING_BUILD_REASON =
  "The automatic index is already running — wait for it to finish";

export function buildBlockReason(
  cliKnownAbsent: boolean,
  phase: LiveIndexPhase | undefined,
): string | null {
  if (cliKnownAbsent) return CLI_MISSING_BUILD_REASON;
  if (phase === "scheduled" || phase === "running") return INDEX_RUNNING_BUILD_REASON;
  return null;
}
