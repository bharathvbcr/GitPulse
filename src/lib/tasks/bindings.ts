import { DEFAULT_FAN_OUT, mapItems } from "../async/pool";
import { formatError } from "../ui/formatError";

/** One worktree's task binding, or why it could not be read. */
export type WorktreeTaskRead =
  | { path: string; taskId: string | null; ok: true }
  | { path: string; taskId: null; ok: false; detail: string };

type TaskCall = <T>(command: string, args: { repoPath: string; worktreePath: string }) => Promise<T>;

/**
 * The task each worktree is bound to, in the order given, at most
 * `DEFAULT_FAN_OUT` requests at a time.
 *
 * Every caller reads bindings through here. One read failing fails only its
 * own row, so a caller can say how many could not be read instead of losing
 * the rest. The Worktrees panel used to read them one at a time, so each
 * worktree waited for the previous one's repository check and ledger lookup.
 */
export function loadWorktreeTasks(
  invokeFn: TaskCall,
  repoPath: string,
  worktreePaths: readonly string[],
): Promise<WorktreeTaskRead[]> {
  return mapItems(worktreePaths, DEFAULT_FAN_OUT, (worktreePath) =>
    invokeFn<string | null>("cmd_worktree_task", { repoPath, worktreePath }).then(
      (taskId): WorktreeTaskRead => ({ path: worktreePath, taskId, ok: true }),
      (error: unknown): WorktreeTaskRead => ({
        path: worktreePath,
        taskId: null,
        ok: false,
        detail: formatError(error),
      }),
    ),
  );
}
