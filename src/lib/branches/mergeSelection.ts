import type { BranchInfo } from "./types";
import type { OperationState } from "../repos/operation";
import { fuzzyMatch } from "./groupBranches";

export interface MergeRequest {
  repoPath: string;
  targetBranch: string | null;
  sourceRef: string;
  ffOnly: boolean;
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
