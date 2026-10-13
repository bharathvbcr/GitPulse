import { derived, type Readable } from "svelte/store";
import { graphStore } from "./graphStore";
import { repoStore, type RepoState } from "./repoStore";

type RepoHead = Pick<RepoState, "currentPath" | "branches">;

interface GraphHead {
  visiblePath: string | null;
  headId: string | null;
}

/**
 * The active repository's HEAD commit, or "" when neither source knows it.
 *
 * Panels that list or measure the working tree re-ask when HEAD moves: it
 * stands in for "the tracked tree changed underneath a clean status" — a
 * checkout, a pull, a commit. On a branch, HEAD is that branch's tip. A
 * detached HEAD belongs to no branch, so the tip is empty and a checkout from
 * one commit to another would change nothing; the graph store's `headId` (the
 * backend's `rev-parse HEAD`, re-read by every refresh, no polling of its own)
 * is the HEAD for the repository it is showing, and it is consulted only then,
 * so an attached HEAD keys on the tip alone and a commit re-asks once, not
 * twice.
 *
 * A string, so a subscriber hears only a real move — not every publish of
 * either store.
 */
export function headTip(repo: RepoHead, graph: GraphHead): string {
  const tip = repo.branches.find((b) => b.is_current && !b.is_remote)?.tip_commit_id ?? "";
  if (tip) return tip;
  return repo.currentPath && graph.visiblePath === repo.currentPath ? (graph.headId ?? "") : "";
}

export function createHeadTipStore(
  repo: Readable<RepoHead>,
  graph: Readable<GraphHead>,
): Readable<string> {
  return derived([repo, graph], ([$repo, $graph]) => headTip($repo, $graph));
}

/** HEAD of the repository on screen, for the panels that re-ask when it moves. */
export const currentHeadTip = createHeadTipStore(repoStore, graphStore);
