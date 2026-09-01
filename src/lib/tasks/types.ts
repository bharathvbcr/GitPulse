/**
 * Wire types for DevCouncil task and lease data.
 *
 * Manvi owns the schema (`crates/dc-store/src/schema.rs`); GitPulse reads it
 * and never writes. These live in their own module because
 * `wire-type-locality-contract` forbids snake_case payload shapes inside
 * `.svelte` files, where `check:types` cannot reach them.
 *
 * Mirrors `src-tauri/src/tasks/mod.rs`.
 */

/** A task's declared scope, as much of it as the gate needs. */
export interface TaskScope {
  id: string;
  title: string;
  status: string;
  /** Paths the plan authorises, repo-relative. */
  planned_files: string[];
  /**
   * Of those, the ones an executor added to its own scope while working.
   *
   * Kept apart from `planned_files` rather than merged: one is what the
   * planner authorised, the other is what the worker authorised for itself.
   * Merging them is what makes a self-granted widening read as an ordinary
   * planned write.
   */
  agent_appended_files: string[];
  forbidden_changes: string[];
  allowed_commands: string[];
}

/** An active lease, as the UI shows it. */
export interface TaskLease {
  task_id: string;
  owner: string;
  agent: string | null;
  branch: string | null;
  status: string;
  created_at: string;
  /**
   * ISO-8601, or null when this lease does not expire.
   *
   * Null means *never expires* — not "expired", not "unknown". Anything
   * computing "safe to reclaim" from expiry must treat null as never
   * reclaimable on that basis.
   */
  expires_at: string | null;
}

/** What GitPulse can see of a repository's DevCouncil state. */
export interface TaskView {
  /**
   * False when this repository has no DevCouncil store — the ordinary case,
   * and not a failure.
   */
  available: boolean;
  store_path: string;
  leases: TaskLease[];
  /**
   * Empty when the store could be read; otherwise why it could not.
   *
   * Separate from `available` so that "no store here" and "a store we failed
   * to read" render differently. Only the second is a problem.
   */
  error: string;
}
