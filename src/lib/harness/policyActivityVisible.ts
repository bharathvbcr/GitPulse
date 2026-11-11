import { harnessPermissionMode } from "./availability";
import type { HarnessStatus } from "../stores/harnessStore";

/**
 * Whether Work → Overview offers the policy journal at all.
 *
 * Hidden only when Manvi is positively known to be absent and the journal is
 * empty: with nothing installed there is nothing to journal. Every other
 * state keeps it, because each is a reason to read it. A status that has not
 * been probed yet, or that failed, is not "no Manvi" (hiding the card then
 * would turn a check that could not run into a clean bill), and a blocked
 * gate or any recorded row is exactly what the journal exists to show.
 */
export function policyActivityVisible(
  harness: HarnessStatus | null,
  actionCount: number,
  statusError: string | null,
): boolean {
  return (
    harnessPermissionMode(harness) !== "unguarded" || actionCount > 0 || statusError !== null
  );
}
