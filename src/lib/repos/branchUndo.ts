import { repoStore } from "../stores/repoStore";
import { toastStore } from "../stores/toastStore";
import type { MutationTarget } from "./storeTypes";

export interface DeletedBranchTip {
  name: string;
  /** The commit the branch pointed at when it was deleted. */
  tip: string;
}

const branches = (count: number) => (count === 1 ? "branch" : "branches");
const described = (tip: DeletedBranchTip) => `${tip.name} at ${tip.tip}`;

/**
 * Tells the user a branch delete went through and offers to undo it.
 *
 * The one owner of branch-delete Undo. Restoring recreates each branch at its
 * old tip, in the repository the delete ran in (`target`, captured before the
 * delete), and only claims what it did: every branch is attempted, a failure
 * does not stop the rest, and a partial or refused restore is an error that
 * names each branch that is still missing with its sha — enough to recreate it
 * by hand. The offer stays on the toast after a failed restore, with a fresh
 * clock, and a retry restores only what is still missing.
 *
 * Two entries with one name can never both be restored. The newest (the last
 * listed) is the one offered; the other is reported by name and sha as not
 * restorable, and the offer never waits on it.
 */
export function offerBranchUndo(message: string, tips: readonly DeletedBranchTip[], target: MutationTarget | null): void {
  if (tips.length === 0) {
    toastStore.success(message);
    return;
  }
  const total = tips.length;
  const newest = new Map<string, DeletedBranchTip>();
  for (const tip of tips) newest.set(tip.name, tip);
  const unrestorable = tips.filter((tip) => newest.get(tip.name) !== tip);
  let missing = [...newest.values()];
  let running = false;
  const id = toastStore.action(message, "Undo", async () => {
    if (running) return false;
    running = true;
    try {
      const failures: string[] = [];
      const stillMissing: DeletedBranchTip[] = [];
      for (const tip of missing) {
        const outcome = await repoStore.createBranch(tip.name, tip.tip, target);
        if (outcome.ok) continue;
        stillMissing.push(tip);
        failures.push(`${described(tip)} (${outcome.error ?? "failed"})`);
      }
      missing = stillMissing;
      const restored = total - unrestorable.length - missing.length;
      const skipped = unrestorable.length
        ? `Not restorable: ${unrestorable.map((tip) => `${described(tip)} (a newer deleted branch has the same name)`).join("; ")}`
        : "";
      if (failures.length === 0) {
        if (!skipped) {
          toastStore.success(total === 1 ? `Restored branch "${tips[0].name}"` : `Restored ${total} branches`);
        } else {
          toastStore.warning(`Restored ${restored} of ${total} ${branches(total)}. ${skipped}`);
        }
        return true;
      }
      toastStore.error(
        `Restored ${restored} of ${total} ${branches(total)}. Failed: ${failures.join("; ")}${skipped ? `. ${skipped}` : ""}`,
      );
      // The offer is still the way back: it gets a full clock from this failure.
      toastStore.restart(id);
      return false;
    } finally {
      running = false;
    }
  });
}
