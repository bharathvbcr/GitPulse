import { repoStore } from "../stores/repoStore";
import { toastStore } from "../stores/toastStore";
import type { MutationTarget } from "./storeTypes";

export interface DeletedBranchTip {
  name: string;
  /** The commit the branch pointed at when it was deleted. */
  tip: string;
}

const branches = (count: number) => (count === 1 ? "branch" : "branches");

/**
 * Tells the user a branch delete went through and offers to undo it.
 *
 * The one owner of branch-delete Undo. Restoring recreates each branch at its
 * old tip, in the repository the delete ran in (`target`, captured before the
 * delete), and only claims what it did: every branch is attempted, a failure
 * does not stop the rest, and a partial or refused restore is an error that
 * names each branch that is still missing with its sha — enough to recreate it
 * by hand. The offer stays on the toast after a failed restore and a retry
 * restores only what is still missing.
 */
export function offerBranchUndo(message: string, tips: readonly DeletedBranchTip[], target: MutationTarget | null): void {
  if (tips.length === 0) {
    toastStore.success(message);
    return;
  }
  const total = tips.length;
  let missing = [...tips];
  let running = false;
  toastStore.action(message, "Undo", async () => {
    if (running) return false;
    running = true;
    try {
      const failures: string[] = [];
      const stillMissing: DeletedBranchTip[] = [];
      for (const tip of missing) {
        const outcome = await repoStore.createBranch(tip.name, tip.tip, target);
        if (outcome.ok) continue;
        stillMissing.push(tip);
        failures.push(`${tip.name} at ${tip.tip} (${outcome.error ?? "failed"})`);
      }
      missing = stillMissing;
      if (failures.length === 0) {
        toastStore.success(total === 1 ? `Restored branch "${tips[0].name}"` : `Restored ${total} branches`);
        return true;
      }
      toastStore.error(
        `Restored ${total - missing.length} of ${total} ${branches(total)}. Failed: ${failures.join("; ")}`,
      );
      return false;
    } finally {
      running = false;
    }
  });
}
