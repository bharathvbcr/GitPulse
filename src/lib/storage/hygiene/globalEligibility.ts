import type { CleanerInventory } from "./globalTypes";

/**
 * The front end's half of one policy: whether a saved inventory offers the host
 * anything to act on, and what to say about coverage when part of the workspace
 * could not be read.
 *
 * This is a module rather than a condition inside `GlobalCleaner.svelte` for the
 * same reason `eligible_candidates` is a free function on the Rust side -- the
 * rule that decides whether a click can delete anything should be testable
 * without a window.
 *
 * `partial` is deliberately not a veto. It means the inventory does not cover
 * everything, which is a statement about the scan and not about any directory in
 * it: a permission denial in a sibling directory, a repository past the linked-
 * worktree sizing cap or a 65th artifact directory all set it while saying
 * nothing about the trees that WERE read to the end. The host refuses on
 * `candidates.is_empty() && run.partial`, so an inventory that holds candidates
 * is one the host will act on; gating the button on `partial` alone greyed out
 * cleanup for the whole workspace and told the user to narrow their roots, while
 * the scheduled sweep -- which never consults this component -- went ahead and
 * cleaned the same trees. Every per-tree and per-repository guard still lives in
 * the host (`scan_complete`, the ignore and tracked-file checks, the candidate
 * cap, the untrusted and scan-error arm, and `quiet_repo` at removal time); this
 * decides only whether the button is worth pressing.
 */
export function cleanupAllowed(inventory: CleanerInventory | null): boolean {
  return !!inventory && inventory.candidates.length > 0;
}

/**
 * The coverage warning for an inventory that could not read everything, or
 * `null` when the scan was complete. Phrased as a limit on what was examined
 * rather than as a refusal, because it is no longer either one.
 */
export function coverageNotice(inventory: CleanerInventory | null): string | null {
  if (!inventory || !inventory.partial) return null;
  if (inventory.candidates.length === 0) {
    return "Incomplete inventory, and nothing eligible was found. Cleanup needs at least one directory it read in full; what could not be read is named below.";
  }
  return "Incomplete inventory: part of the workspace could not be read. That does not hold up the rest — every directory listed below was read in full and stays eligible. What could not be read is named below and is left alone.";
}
