import { bindResourceBudgets } from "../resourceBudgets";
import { gpuPolicy, gpuSlots } from "../terminal/gpuRenderer";
import { graphStore } from "./graphStore";
import { resourceProfile } from "./hostLoadStore";

let following: (() => void) | null = null;

/**
 * Makes the terminal's GPU slot pool and grace and the graph store's cache
 * limit follow the resource profile, once per page. Until the first status
 * arrives they hold the compact budgets they were created with. Safe to call
 * from every component that starts host load.
 */
export function startResourceBudgets(): void {
  if (following) return;
  following = bindResourceBudgets(resourceProfile, {
    slots: gpuSlots,
    policy: gpuPolicy,
    graph: graphStore,
  });
}

/** Test seam: stops following so the next start binds again. */
export function stopResourceBudgetsForTests(): void {
  following?.();
  following = null;
}
