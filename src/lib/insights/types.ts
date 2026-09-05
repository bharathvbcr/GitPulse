/**
 * Wire shapes matching `src-tauri/src/insights/mod.rs`.
 */

import type { CodeintelStatus } from "../codeintel/types";
import type { LedgerStatus } from "../ledger/types";
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
}

export interface AgentKindCount {
  kind: string;
  sessions: number;
}

export interface AgentSummary {
  /** False when the session listing failed. `sessions: 0` then means unknown. */
  ok: boolean;
  sessions: number;
  kinds: AgentKindCount[];
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

export interface CollisionItem {
  path: string;
  worktrees: CollisionParty[];
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
  plugin_found: boolean;
  plugin_path: string;
  plugin_error: string;
  plugin_manifest_json: string;
  plugin_mcp_json: string;
  tools: McpToolInfo[];
}
