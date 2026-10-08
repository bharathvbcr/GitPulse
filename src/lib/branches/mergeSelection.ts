import type { BranchInfo } from "./types";
import type { OperationState } from "../repos/operation";
import { fuzzyMatch } from "./groupBranches";

/**
 * How a merge joins the source into the checked-out branch. Mirrors the Rust
 * `MergeMode` (kebab-case on the wire).
 */
export type MergeMode = "default" | "ff-only" | "no-ff" | "squash";

export const MERGE_MODES: readonly { mode: MergeMode; label: string; detail: string }[] = [
  { mode: "default", label: "Merge", detail: "Fast-forward when possible, otherwise record a merge commit." },
  { mode: "ff-only", label: "Fast-forward only", detail: "Only move the branch forward; stop if a merge commit is needed." },
  { mode: "no-ff", label: "Always create a merge commit", detail: "Record a merge commit even when a fast-forward was possible (--no-ff)." },
  { mode: "squash", label: "Squash into one commit", detail: "Combine the source's changes into a single new commit with no merge parent (--squash)." },
];

export interface MergeRequest {
  repoPath: string;
  targetBranch: string | null;
  sourceRef: string;
  mode: MergeMode;
}

/** Fully qualified refs preserve remote identity and avoid tag/name ambiguity. */
export function mergeRef(branch: BranchInfo): string {
  return `${branch.is_remote ? "refs/remotes/" : "refs/heads/"}${branch.name}`;
}

export function mergeCandidates(branches: BranchInfo[], currentBranch: string | null, query = ""): BranchInfo[] {
  const seen = new Set<string>();
  const exact = query.trim();
  const folded = exact.toLowerCase();
  return branches.filter((branch) => {
    const ref = mergeRef(branch);
    if (!branch.name || branch.is_current || (!branch.is_remote && branch.name === currentBranch)
      || (branch.is_remote && branch.name.endsWith("/HEAD")) || seen.has(ref)) return false;
    seen.add(ref);
    return fuzzyMatch(query, branch.name);
  }).sort((a, b) =>
    Number(b.name === exact) - Number(a.name === exact)
    || Number(b.name.toLowerCase() === folded) - Number(a.name.toLowerCase() === folded)
    || Number(a.is_remote) - Number(b.is_remote)
    || a.name.localeCompare(b.name));
}

export function mergeBlockedReason(
  state: {
    currentPath: string | null;
    currentBranch: string | null;
    isBare: boolean;
    isLoading: boolean;
    operation: OperationState;
  },
  request: MergeRequest,
): string | null {
  if (!state.currentPath || state.currentPath !== request.repoPath) return "The repository changed. Close this dialog and choose a branch again.";
  if (state.isBare) return "Open a working copy to merge branches.";
  if (state.isLoading) return "Waiting for repository information…";
  if (!state.currentBranch) return "Check out a local branch before merging.";
  if (state.currentBranch !== request.targetBranch) return "The checked-out branch changed. Close this dialog and review the new destination.";
  if (state.operation.probeFailed) return "Could not check for an operation in progress. Refresh the repository and try again.";
  if (state.operation.operation) return "Finish or abort the operation in progress before starting another merge.";
  return null;
}
