import type { TaskScope } from "./types";

/** Most ids one `cmd_task_scopes` call accepts; `tasks::MAX_SCOPE_IDS` in Rust. */
export const MAX_SCOPE_IDS = 64;

type ScopeCall = <T>(command: string, args: { repoPath: string; taskIds: string[] }) => Promise<T>;

/**
 * The scopes of `taskIds`, keyed by id. A task the store does not hold is
 * absent from the record.
 *
 * Every caller goes through here rather than asking per task: finding the
 * store authenticates the repository with a `git worktree list`, so one call
 * per task was one git process per task on every refresh. A list longer than
 * the Rust limit is read in chunks, all of it, not cut at the limit.
 */
export async function loadTaskScopes(
  invokeFn: ScopeCall,
  repoPath: string,
  taskIds: readonly string[],
): Promise<Record<string, TaskScope>> {
  const unique = [...new Set(taskIds)];
  const found: Record<string, TaskScope> = Object.create(null);
  for (let start = 0; start < unique.length; start += MAX_SCOPE_IDS) {
    const scopes = await invokeFn<TaskScope[]>("cmd_task_scopes", {
      repoPath,
      taskIds: unique.slice(start, start + MAX_SCOPE_IDS),
    });
    for (const scope of scopes) found[scope.id] = scope;
  }
  return found;
}
