/**
 * Wire shapes matching `src-tauri/src/insights/mod.rs`.
 */

import type { CodeintelStatus } from "../codeintel/types";
import type { Freshness, LedgerStatus } from "../ledger/types";
import type { RepoOperation } from "../repos/operation";

export interface WorktreeSummary {
  path: string;
  name: string;
  /** Null when the branch could not be read; `is_detached` says which. */
  branch: string | null;
  /** True only when HEAD is genuinely detached, not when the read failed. */
  is_detached: boolean;
  is_main: boolean;
  is_bare: boolean;
  /** Null when this worktree was never scanned. Never treat null as clean. */
  dirty_files: number | null;
  agent_kind: string;
  session_slug: string;
  operation_kind: string;
  /** False when the parked-operation probe did not run. */
  operation_ok: boolean;
  /** The commit this checkout is on, detached or not. Null when none was named. */
  head?: string | null;
  /**
   * Commits on `head` that the default base lacks: committed-but-unmerged
   * work, which `dirty_files` stops showing once a lane commits. Null is
   * "not measured" (see `divergence_note`), never zero.
   */
  ahead?: number | null;
  /** Commits on the default base that `head` lacks. Null when not measured. */
  behind?: number | null;
  /** Commits since `head` left the default base; the same count as `ahead`. */
  commits_since_base?: number | null;
  /** Why the three counts are absent for a checkout that has a `head`. */
  divergence_note?: string;
}

export interface AgentKindCount {
  kind: string;
  sessions: number;
}

/** One running agent session, as its agent registered it. */
export interface LiveSession {
  kind: string;
  pid: number;
  /** How the session was started (`cli`, `claude-desktop`, …), verbatim. */
  entrypoint: string;
  /** The agent's own last-reported state (`busy`, `idle`, …), verbatim. */
  status: string;
  cwd: string;
}

export interface LiveWorktree {
  path: string;
  sessions: LiveSession[];
}

export interface LiveKindStatus {
  kind: string;
  /** False when this kind cannot be observed. `sessions: 0` then means unknown. */
  ok: boolean;
  error: string;
  sessions: number;
  /** Entries that may be live here but could not be verified; makes `sessions` a floor. */
  unverified: number;
  truncated: boolean;
}

/** Running agent sessions per worktree path, from each agent's own registry. */
export interface LiveSessionFacet {
  /** True only when every kind was observed completely. Otherwise `sessions` is a floor. */
  ok: boolean;
  sessions: number;
  kinds: LiveKindStatus[];
  worktrees: LiveWorktree[];
}

export interface AgentSummary {
  /** False when the session listing failed. `sessions: 0` then means unknown. */
  ok: boolean;
  /** Worktrees laid out for an agent, by layout — not running sessions; see `live`. */
  sessions: number;
  kinds: AgentKindCount[];
  live: LiveSessionFacet;
  /**
   * True when these numbers came from a capped sample of the repository's
   * worktrees.
   *
   * `sessions` is then a floor, and a kind whose only worktrees fall past the
   * cap is absent from `kinds` rather than undercounted. Render the count as
   * "at least n" — a bounded sample shown as an exact total is the reporting
   * bug this field exists to prevent.
   */
  truncated: boolean;
}

export interface WorktreeFacet {
  ok: boolean;
  error: string;
  count: number;
  /** Worktrees actually scanned. `dirty` is only meaningful against this. */
  scanned: number;
  dirty: number;
  /** Scanned-but-unknown. Counting these as clean is the bug this exists for. */
  dirty_unknown: number;
  blocked: number;
  blocked_unknown: number;
  truncated: boolean;
  items: WorktreeSummary[];
}

export interface ChangesFacet {
  ok: boolean;
  error: string;
  files: number;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  additions: number;
  deletions: number;
  /** Rows whose churn could not be parsed; their 0/0 is not a measurement. */
  churn_warnings: number;
  /** True when the churn totals saturated, so they understate reality. */
  churn_overflowed: boolean;
  truncated: boolean;
}

export interface CollisionParty {
  path: string;
  branch: string | null;
  agent_kind: string;
}

export type EntityCollisionKind = "shared_symbol" | "disjoint_symbols" | "file_level";

export interface EntityCollisionVerdict {
  path: string;
  kind: EntityCollisionKind;
  reason: string;
  shared_symbols: string[];
}

export interface CollisionItem {
  path: string;
  worktrees: CollisionParty[];
  /** Present on the first overlapping row when symbol classification ran. */
  entity?: EntityCollisionVerdict;
}

/** A worktree two or more live sessions share, and the files dirty there. */
export interface SharedWorktree {
  path: string;
  branch: string | null;
  sessions: LiveSession[];
  files: string[];
  /** False when this worktree's dirty files were not read. Empty `files` then means unknown. */
  scanned: boolean;
  truncated: boolean;
}

export interface CollisionRisk {
  ok: boolean;
  error: string;
  overlapping_files: number;
  worktrees_involved: number;
  scanned_worktrees: number;
  /** Never attempted (past the scan cap). */
  unscanned_worktrees: number;
  /** Attempted and errored. Absent from `items` despite possibly colliding. */
  failed_worktrees: number;
  truncated: boolean;
  items: CollisionItem[];
  /** Dirty files in worktrees two live sessions share — invisible to `overlapping_files`. */
  shared_worktree_files: number;
  shared_worktrees: SharedWorktree[];
  /** False when some agent kind could not be observed; empty `shared_worktrees` is then not clean. */
  sessions_ok: boolean;
  sessions_error: string;
}

export interface InsightsSnapshot {
  repo_path: string;
  branch: string | null;
  /** False when the branch could not be read, as opposed to detached HEAD. */
  branch_ok: boolean;
  /** True when the snapshot hit its deadline, so facets below may be partial. */
  deadline_expired: boolean;
  duration_ms: number;
  worktrees: WorktreeFacet;
  agents: AgentSummary;
  changes: ChangesFacet;
  collisions: CollisionRisk;
  ledger: LedgerStatus;
  codeintel: CodeintelStatus;
}

export interface ChangedFile {
  path: string;
  status_code: string;
  is_staged: boolean;
  is_conflicted: boolean;
  additions: number;
  deletions: number;
  /**
   * Non-empty when additions/deletions may understate this row's real churn.
   * Omitted from the wire entirely while empty, so absent and `[]` mean the
   * same thing: nothing was wrong with this row.
   */
  warnings?: string[];
}

export interface ActiveChanges {
  repo_path: string;
  worktree_path: string;
  ok: boolean;
  error: string;
  files: ChangedFile[];
  total: number;
  shown: number;
  truncated: boolean;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  additions: number;
  deletions: number;
  churn_warnings: number;
  churn_overflowed: boolean;
}

/**
 * Five independent probes, each reporting whether it ran.
 *
 * Only `changes` used to carry an `ok`; the other four failed into empty
 * values, so a context read against an unreachable repository was
 * indistinguishable from a clean one with nothing bound to it.
 */
export interface ChangeContext {
  repo_path: string;
  worktree: WorktreeSummary;
  worktree_ok: boolean;
  worktree_error: string;
  task_id: string;
  task_ok: boolean;
  task_error: string;
  changes: ActiveChanges;
  /** The whole scan, not just its rows: an empty `items` needs its coverage. */
  collisions: CollisionRisk;
  operation: RepoOperation | null;
  operation_ok: boolean;
  operation_error: string;
  /**
   * How current the ledger this context read is. `as_of` is the `ts_utc` of the
   * newest row it could see; `stale` is true when nothing shows a recorder ran
   * within `stale_after_secs`, or when freshness could not be read.
   */
  ledger_freshness: Freshness;
}

export interface McpToolInfo {
  name: string;
  title: string;
  description: string;
}

export interface McpInfo {
  protocol_version: string;
  server_name: string;
  server_version: string;
  read_only: boolean;
  binary_found: boolean;
  binary_path: string;
  binary_error: string;
  /** The bare `gitpulse-mcp` the plugin manifest names resolves on PATH. */
  binary_on_path: boolean;
  hook_found: boolean;
  hook_path: string;
  hook_error: string;
  hook_on_path: boolean;
  plugin_found: boolean;
  plugin_path: string;
  plugin_error: string;
  plugin_manifest_json: string;
  plugin_mcp_json: string;
  /** MCP manifest to install: `command` is the resolved absolute path; empty when not found. */
  mcp_config_json: string;
  /** Hooks manifest to install: absolute path, deciding hooks refuse when the binary is missing; empty when not found. */
  hooks_config_json: string;
  tools: McpToolInfo[];
}

/**
 * What merging two commits would do (`MergeCandidate`): three states, and
 * `unknown` is never to be read as `clean`.
 */
export type MergeCandidate =
  | { state: "clean"; tree: string }
  | { state: "conflict"; paths: string[]; paths_total: number }
  | { state: "unknown"; reason: string };

/** One pair of checkouts and what merging their HEADs would do. */
export interface PairVerdict {
  a_path: string;
  b_path: string;
  a_head: string;
  b_head: string;
  verdict: MergeCandidate;
}

/** `gitpulse_merge_conflicts`: wire shape of `insights::merge_conflicts::MergeConflicts`. */
export interface MergeConflicts {
  /** False when the answer is incomplete (past the pair cap) or unavailable. */
  ok: boolean;
  error: string;
  worktrees: number;
  pairs_total: number;
  pairs_checked: number;
  same_head_pairs: number;
  clean: number;
  conflict: number;
  unknown: number;
  /** True when `pairs_checked < pairs_total`: the counts are a floor. */
  truncated: boolean;
  items: PairVerdict[];
}
