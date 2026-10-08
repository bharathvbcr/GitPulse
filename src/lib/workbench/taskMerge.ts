/**
 * Whether a board selection can be merged, and into which repository.
 *
 * The merge itself is the host's (`items.merge`, the same code as
 * `gitpulse_merge_tasks`); this only decides when the board offers it, so a
 * button is never enabled for a selection the merge would refuse on sight.
 * The merge refuses a task linked to more than one repository — deleting it
 * would delete it from the others too — so a selection qualifies only when
 * every card is linked to exactly one repository, the same one.
 */
import type { TaskCard } from "./client";

/** The merge's bound: 25 sources (intake `MAX_MERGE_SOURCES`) and the target. Pinned by `taskMerge.test.ts`. */
export const MAX_MERGE_CARDS = 26;

export type MergeEligibility =
  | { ok: true; repositoryId: string }
  | { ok: false; reason: string };

export function mergeEligibility(cards: readonly Pick<TaskCard, "repository_ids">[]): MergeEligibility {
  if (cards.length < 2) return { ok: false, reason: "Select two or more tasks to merge." };
  if (cards.length > MAX_MERGE_CARDS) return { ok: false, reason: `Merge at most ${MAX_MERGE_CARDS} tasks at once.` };
  const first = cards[0].repository_ids;
  if (first.length !== 1) return { ok: false, reason: "A task linked to several repositories cannot be merged; it would be deleted from all of them." };
  const repositoryId = first[0];
  for (const card of cards) {
    if (card.repository_ids.length !== 1) return { ok: false, reason: "A task linked to several repositories cannot be merged; it would be deleted from all of them." };
    if (card.repository_ids[0] !== repositoryId) return { ok: false, reason: "Tasks in different repositories cannot be merged." };
  }
  return { ok: true, repositoryId };
}
