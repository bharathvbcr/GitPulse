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
]);
