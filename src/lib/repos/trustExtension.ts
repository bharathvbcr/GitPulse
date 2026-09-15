import { writable } from "svelte/store";

/**
 * Announced when an approval is extended to a repository's worktrees.
 *
 * The offer and the rows that depend on it are no longer in one component.
 * The banner sits above the branch list so it cannot scroll out of sight,
 * while the worktree rows stay below it — and a grant made up there has to
 * reach them, or the panel keeps showing the gaps the grant just closed.
 *
 * `repoStore.refresh` cannot carry this. It re-hydrates a session under its
 * existing `generation`, and `generation` is what panels compare to tell a
 * stale response from a current one; nothing keyed on it re-runs, by design.
 * So the signal is its own thing rather than a bend in that one.
 *
 * A counter, not a flag: two extensions in one session are two events, and a
 * boolean that has to be reset has a window in which it is wrong.
 */
export const trustExtended = writable(0);

/** Call after a grant that actually widened what can be read. */
export function announceTrustExtended(): void {
  trustExtended.update((count) => count + 1);
}
