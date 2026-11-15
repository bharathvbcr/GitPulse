/**
 * The capacity budgets the resource profile hands the app
 * (`src-tauri/src/sysload/profile.rs`).
 *
 * Capacity only: how many WebGL terminal contexts may be live, how long a
 * hidden terminal keeps its context, how many repositories keep their graph
 * rows. Protocol constants and stored settings are not budgets and are not
 * here. The `roomy` tier is what the app did before there was a profile
 * (`MAX_VISIBLE_SESSIONS`, `GPU_HIDE_GRACE_MS`, `GRAPH_CACHE_LIMIT`), and
 * `resourceBudgets.test.ts` keeps the two sides equal.
 *
 * This module imports nothing from the app so the consumers it sizes (the
 * terminal's slot pool, the graph store) can take their starting values from
 * it without a cycle.
 */

/** `sysload::ResourceBudgets`. */
export interface ResourceBudgets {
  /** WebGL terminal contexts live at once. */
  gpu_contexts: number;
  /** How long a hidden terminal keeps its context, in milliseconds. */
  gpu_hide_grace_ms: number;
  /** Repositories whose commit-graph rows stay in memory. */
  graph_cache_repositories: number;
}

/**
 * What every consumer uses until the first status arrives. A machine with
 * 8 GB must not allocate roomy-sized pools during startup, so "not known yet"
 * is the smallest tier, never the largest.
 */
export const COMPACT_BUDGETS: ResourceBudgets = {
  gpu_contexts: 2,
  gpu_hide_grace_ms: 15_000,
  graph_cache_repositories: 2,
};

/** The consumers a profile sizes, each through the one bound it already has. */
export interface ResourceBudgetTargets {
  slots: { setCap(cap: number): void };
  policy: { hideGraceMs: number };
  graph: { setCacheLimit(limit: number): void };
}

/**
 * Hands each consumer its budget. A field that is not a finite number is
 * skipped, so one bad value from a backend cannot stop the others applying;
 * counts are floored and never below one, a grace never below zero.
 */
export function applyResourceBudgets(budgets: ResourceBudgets, targets: ResourceBudgetTargets): void {
  if (Number.isFinite(budgets.gpu_contexts)) {
    targets.slots.setCap(Math.max(1, Math.floor(budgets.gpu_contexts)));
  }
  if (Number.isFinite(budgets.gpu_hide_grace_ms)) {
    targets.policy.hideGraceMs = Math.max(0, Math.floor(budgets.gpu_hide_grace_ms));
  }
  if (Number.isFinite(budgets.graph_cache_repositories)) {
    targets.graph.setCacheLimit(Math.max(1, Math.floor(budgets.graph_cache_repositories)));
  }
}

/**
 * Applies the profile's budgets now and on every change; null (no status yet)
 * applies the compact budgets. Returns the function that stops following.
 * Re-applying the same budgets is a no-op for every target.
 */
export function bindResourceBudgets(
  profile: { subscribe(run: (value: { budgets: ResourceBudgets } | null) => void): () => void },
  targets: ResourceBudgetTargets,
): () => void {
  return profile.subscribe((value) => applyResourceBudgets(value?.budgets ?? COMPACT_BUDGETS, targets));
}
