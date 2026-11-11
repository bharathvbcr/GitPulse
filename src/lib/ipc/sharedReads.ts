/**
 * The commands whose identical concurrent calls `invoke` answers with one.
 *
 * Two callers asking the same read at the same moment (a watcher refresh and
 * a status poll both wanting `cmd_get_status`, two panels both wanting the
 * branch list) used to reach the backend twice, each paying for its own git
 * children. The backend shares identical reads that are still QUEUED
 * (`run_read_shared`); it cannot share one that has already started. This list
 * is the frontend half: a call whose command is named here and whose arguments
 * are plain data joins the same call already in flight.
 *
 * A call joins only a read that started in the SAME join generation (see
 * `readGeneration` in invoke.ts). Every command not named here ends a
 * generation when it is sent and again when it settles, and
 * `invalidateSharedReads()` ends one on a watcher event, so a read that began
 * before a change is never handed to a caller asking after it. The backend's
 * `run_read_shared` is sound for the same reason: it shares only reads that
 * have not started.
 *
 * An allowlist, never a pattern. Joining is only correct for a command that
 * changes nothing the second caller would have seen differently, so a command
 * earns its place by being read through its handler, and a new command is not
 * joined until someone has done that. A write is never named here: two
 * identical writes are two writes.
 *
 * Every entry is a pure read of repository state. `cmd_branch_stats` also
 * fills the oid-keyed churn cache, which is idempotent by construction (the
 * key is the object id).
 */
/**
 * The reads that accept a `requestId`, so the backend can stop one whose
 * callers have all gone (`engine/git_cli/read_cancel.rs`).
 *
 * Only reads a newer click really supersedes and whose git child runs long
 * enough for the kill to matter: the graph window and the commit, compare and
 * range diffs. Each is called with an abort signal by exactly one store
 * (`graphStore`, `diffSelection`); a read nobody aborts gains nothing from an
 * id, so it is not listed. The id is only ever sent for a call that carries a
 * signal, and `invoke` adds it under its own name, outside the arguments the
 * call is joined and retried by.
 */
export const CANCELLABLE_READ_COMMANDS: ReadonlySet<string> = new Set([
  "cmd_get_commit_graph",
  "cmd_get_commit_diff",
  "cmd_get_commit_file_diff",
  "cmd_get_range_diff",
  "cmd_get_compare_diff",
]);

/**
 * Stops the read that went out under `requestId`; the backend treats an id that
 * has ended, or was never known, as a no-op. Written as a call here, with the
 * command as a literal, so `check:ipc` and the argument contract read it like
 * any other call site, while `invoke.ts` (which the status popover also loads)
 * receives it by injection and carries no command name.
 */
export function cancelRead(
  invoke: (cmd: string, args: { requestId: string }) => Promise<unknown>,
  requestId: string,
): Promise<unknown> {
  return invoke("cmd_cancel_read", { requestId });
}

export const SHARED_READ_COMMANDS: ReadonlySet<string> = new Set([
  "cmd_get_status",
  "cmd_list_branches",
  "cmd_list_tags",
  "cmd_repo_operation",
  "cmd_stash_list",
  "cmd_last_fetch_at",
  "cmd_branch_stats",
  "cmd_get_commit_graph",
  "cmd_get_commit_details",
  // The history rail's hover card: `git log --no-walk`, pure reads.
  "cmd_get_commit_notes",
  "cmd_get_commit_bodies",
]);
