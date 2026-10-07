/**
 * Payloads for the events the backend emits about a repository.
 *
 * Events are a second wire surface, separate from command returns, and this
 * one was consumed as an anonymous `{ path?: string }` in App.svelte — which
 * also had it optional, though Rust always sends it.
 */

/** What a settled change touched; see `ChangeKind` in `watcher/mod.rs`. */
export type ChangeKind =
  | "refs"
  | "index"
  | "config"
  | "ignore"
  | "objects"
  | "git_state"
  | "worktree"
  | "documents"
  | "unknown";

/**
 * Read from the same paths the watcher's noise gate admitted. `paths` names
 * top-level worktree entries only (the worktree watch is non-recursive), so
 * it means "at least these", never "only these".
 */
export interface RepoChange {
  kinds: ChangeKind[];
  paths: string[];
  paths_truncated: boolean;
}

/** Emitted when the filesystem watcher sees the repository change. */
export interface RepoChangedPayload {
  path: string;
  change: RepoChange;
}

/**
 * Emitted once when a watched repository's git directory disappears; its
 * watch has ended. `RepoGonePayload` in `watcher/mod.rs`.
 */
export interface RepoGonePayload {
  path: string;
}
