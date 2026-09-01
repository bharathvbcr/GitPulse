/**
 * Wire types for the stacked-branch hierarchy.
 *
 * These were declared inside CodeStackViewer under shorter names — StackNode
 * and StackPayload — with the breadcrumb as an anonymous object. Same shapes,
 * different names, so `check:types` had nothing it could pair them with. They
 * now carry their Rust names, which is what puts the payload under contract.
 */

/** One branch in the stack, and its position relative to its parent. */
export interface StackedBranchNode {
  branch_name: string;
  tip_commit_id: string;
  parent_branch_name?: string | null;
  child_branch_names: string[];
  commit_count_ahead_of_parent: number;
}

/** The path from the root branch down to the current one. */
export interface BranchAncestryChain {
  current_branch: string;
  /** Typically "main" or "master". */
  root_branch: string;
  /** e.g. ["main", "feat-auth", "feat-oauth-google"]. */
  breadcrumb_chain: string[];
}

/** Everything the Stack page renders in one IPC round trip. */
export interface StackHierarchyPayload {
  nodes: StackedBranchNode[];
  default_branch: string;
  breadcrumb: BranchAncestryChain;
}
