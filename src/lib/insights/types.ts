/**
 * Wire shapes matching `src-tauri/src/insights/mod.rs`.
 */

import type { CodeintelStatus } from "../codeintel/types";
import type { LedgerStatus } from "../ledger/types";
import type { RepoOperation } from "../repos/operation";

export interface WorktreeSummary {
  path: string;
  name: string;
  branch: string | null;
  is_main: boolean;
  is_bare: boolean;
  dirty_files: number | null;
  agent_kind: string;
  session_slug: string;
  operation_kind: string;
}

export interface AgentKindCount {
  kind: string;
  sessions: number;
}

export interface AgentSummary {
  sessions: number;
  kinds: AgentKindCount[];
}

export interface WorktreeFacet {
  ok: boolean;
  error: string;
  count: number;
  dirty: number;
  blocked: number;
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
  unscanned_worktrees: number;
  truncated: boolean;
  items: CollisionItem[];
}

export interface InsightsSnapshot {
  repo_path: string;
  branch: string | null;
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
}

export interface ChangeContext {
  repo_path: string;
  worktree: WorktreeSummary;
  task_id: string;
  changes: ActiveChanges;
  collisions: CollisionItem[];
  operation: RepoOperation | null;
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
