/**
 * Decides what the DiffViewer's whitespace toggle must refetch.
 *
 * The toggle used to hardcode `isStaged = false`, silently swapping a staged
 * diff for the unstaged one (and a commit diff for the worktree diff). The
 * staged-ness of the selection lives in the session's status list — the same
 * flag Sidebar.svelte passes to `selectFileDiff`.
 */
export interface WhitespaceToggleDecision {
  /** False when there is no worktree diff behind the selection to refetch. */
  refetch: boolean;
  isStaged: boolean;
}

interface SelectionInput {
  filePath: string | null | undefined;
  /** A commit/range selection has no worktree diff; toggling must not swap it. */
  commitId: string | null | undefined;
  statuses: ReadonlyArray<{ path: string; is_staged: boolean }>;
  isStaged?: boolean;
}

export function decideWhitespaceRefetch(input: SelectionInput): WhitespaceToggleDecision {
  const { filePath, commitId, statuses, isStaged } = input;
  if (!filePath || commitId) {
    return { refetch: false, isStaged: false };
  }
  if (typeof isStaged === "boolean") {
    return { refetch: true, isStaged };
  }
  // A path can appear twice (staged + unstaged entries). The staged entry is
  // picked explicitly rather than relying on list order: Sidebar.svelte may
  // offer either side first, and the toggle must keep whichever side carries
  // the staged diff.
  const match =
    statuses.find((status) => status.path === filePath && status.is_staged) ??
    statuses.find((status) => status.path === filePath);
  if (!match) {
    return { refetch: false, isStaged: false };
  }
  return { refetch: true, isStaged: match.is_staged };
}
