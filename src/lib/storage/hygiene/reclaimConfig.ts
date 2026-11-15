import type { CleanerConfig, CloneBudget, ReclaimScopes } from "./globalTypes";

/** The backend's defaults. The wire omits `scopes` and `clone_budget` while they sit at them. */
export const DEFAULT_SCOPES: ReclaimScopes = Object.freeze({
  worktree_caches: false,
  per_tree_vetoes: false,
  prune_worktrees: false,
  worktree_idle_days: 7,
});
export const DEFAULT_CLONE_BUDGET: CloneBudget = Object.freeze({ max_target_clones: 0, max_private_bytes: 50 * 1024 ** 3 });

/** A config's scopes, with a missing field read as the default rather than as undefined. */
export function scopesOf(config: Pick<CleanerConfig, "scopes">): ReclaimScopes {
  return { ...DEFAULT_SCOPES, ...(config.scopes ?? {}) };
}
export function cloneBudgetOf(config: Pick<CleanerConfig, "clone_budget">): CloneBudget {
  return { ...DEFAULT_CLONE_BUDGET, ...(config.clone_budget ?? {}) };
}
/** Whether any scope that widens what the cleaner may delete is on. */
export function anyScopeOn(scopes: ReclaimScopes): boolean {
  return scopes.worktree_caches || scopes.per_tree_vetoes || scopes.prune_worktrees;
}
/** Sum of `private_bytes` over the worktrees that were sized; unsized ones add nothing. */
export function privateBytesTotal(worktrees: ReadonlyArray<{ private_bytes?: number }>): { total: number; sized: number } {
  let total = 0, sized = 0;
  for (const w of worktrees) if (w.private_bytes !== undefined) { total += w.private_bytes; sized++; }
  return { total, sized };
}
/** The scope tag an inventory row shows. */
export function scopeLabel(scope: string): string {
  if (scope === "worktree-cache") return "worktree cache";
  if (scope === "prune-worktree") return "stale worktree";
  return scope || "repository";
}
