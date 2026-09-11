/**
 * The file list that travels WITH the diff.
 *
 * Selecting a file used to be a one-way trip. The working-tree path was fine —
 * the Sidebar lists those files and stays on screen — but a commit's files are
 * listed by `CommitDetails`, which lives inside the Graph view only. So
 * clicking one switched to the Diff view, the list vanished with the view that
 * owned it, and reading a second file of the same commit meant going back to
 * Graph, finding the commit again, and clicking the next row. For a ten-file
 * commit that is nine round trips to read one change.
 *
 * This module answers "what else could I be looking at from here", from
 * whichever selection is live, so the Diff view can carry its own list and the
 * trip becomes one click.
 *
 * It is pure: the commit's files are already in `graphStore.selectedCommitDetails`
 * and the working tree's are already in `repoStore.statuses`, so the rail costs
 * no extra IPC and can never disagree with the panes it mirrors.
 */

/** Where a rail's entries came from. `none` renders no rail at all. */
import { hasUnstagedChanges, statusForSide } from "../files/fileStatus";
export type RailSource = "commit" | "worktree" | "none";

export interface RailEntry {
  path: string;
  /** Set when the file was renamed, so the rail can show `old → new`. */
  oldPath?: string;
  /** Git's status letter (`M`, `A`, `D`, `R`…), or empty when unknown. */
  statusCode: string;
  additions: number;
  deletions: number;
  /**
   * Which side of the index this entry is. Only meaningful for `worktree`
   * rails, where the same path can appear staged AND unstaged with different
   * content — opening the wrong one shows a diff the user did not ask for.
   */
  isStaged: boolean;
}

export interface FileRail {
  source: RailSource;
  entries: RailEntry[];
  /**
   * True when the backing list was cut short. The rail must say so: a
   * truncated list rendered as a complete one tells the reader they have seen
   * every file in the commit when they have seen the first fifty.
   */
  truncated: boolean;
  /** The real file count when known and larger than `entries`; else 0. */
  totalCount: number;
}

/** Shape of `CommitFileChange`, structurally so this module imports no store. */
export interface CommitFileLike {
  path: string;
  status_code: string;
  additions: number;
  deletions: number;
}

/** Shape of `FileStatus`, structurally, for the same reason. */
export interface WorktreeFileLike {
  path: string;
  /** Null and absent both mean "not renamed"; the wire uses both. */
  old_path?: string | null;
  status_code: string;
  is_staged: boolean;
  additions: number;
  deletions: number;
  staged_additions?: number;
  staged_deletions?: number;
  unstaged_additions?: number;
  unstaged_deletions?: number;
}

export interface RailInput {
  /** How the current selection was made. */
  selectionKind: "file" | "commit" | "range";
  /** The commit's changed files, when a commit is selected. */
  commitFiles: readonly CommitFileLike[] | null;
  /** Whether the backend cut the commit's file list short. */
  commitFilesTruncated: boolean;
  /** The commit's true file count, when the list was cut short. */
  commitFilesTotal: number;
  /** The working tree's changed files. */
  statuses: readonly WorktreeFileLike[];
}

export const EMPTY_RAIL: FileRail = {
  source: "none",
  entries: [],
  truncated: false,
  totalCount: 0,
};

/**
 * Builds the rail for the current selection.
 *
 * A range diff (`from..to`) gets no rail: the backend returns one combined
 * patch, not a per-file list, and inventing entries by parsing the diff text
 * would produce a list that silently disagrees with what is on screen.
 */
export function buildFileRail(input: RailInput): FileRail {
  if (input.selectionKind === "commit") {
    const files = input.commitFiles;
    // Null means the details have not landed yet — distinct from a commit
    // that genuinely changed nothing, which is why the rail renders nothing
    // rather than an empty list claiming the commit is empty.
    if (!files || files.length === 0) return EMPTY_RAIL;
    return {
      source: "commit",
      entries: files.map((file) => ({
        path: file.path,
        statusCode: file.status_code,
        additions: file.additions,
        deletions: file.deletions,
        isStaged: false,
      })),
      truncated: input.commitFilesTruncated,
      totalCount:
        input.commitFilesTruncated && input.commitFilesTotal > files.length
          ? input.commitFilesTotal
          : 0,
    };
  }

  if (input.selectionKind === "file") {
    const sides = input.statuses.flatMap((file) => file.is_staged && hasUnstagedChanges(file)
      ? [statusForSide(file, true), statusForSide(file, false)]
      : [statusForSide(file, file.is_staged)]);
    const entries = sides.map((file) => ({
      path: file.path,
      oldPath: file.old_path ?? undefined,
      statusCode: file.status_code,
      additions: file.additions,
      deletions: file.deletions,
      isStaged: file.is_staged,
    }));
    if (entries.length === 0) return EMPTY_RAIL;
    return { source: "worktree", entries, truncated: false, totalCount: 0 };
  }

  return EMPTY_RAIL;
}

/**
 * Identity of one rail entry.
 *
 * Path alone is not enough on a worktree rail: staging part of a file leaves
 * the same path on both sides of the index with different content, and keying
 * on path would make the two indistinguishable — the rail would highlight the
 * wrong row and stepping would skip one of them.
 */
export function entryKey(entry: Pick<RailEntry, "path" | "isStaged">): string {
  return `${entry.isStaged ? "staged" : "worktree"}\u0000${entry.path}`;
}

/** Whether `entry` is the file currently on screen. */
export function isCurrent(
  entry: RailEntry,
  currentPath: string | null,
  currentIsStaged: boolean,
  source: RailSource,
): boolean {
  if (!currentPath || entry.path !== currentPath) return false;
  // A commit rail has no staged/unstaged split, so the path decides alone.
  return source !== "worktree" || entry.isStaged === currentIsStaged;
}

/**
 * The entry `delta` steps away from the current one.
 *
 * Deliberately does NOT wrap. Wrapping at the end of a commit's files sends
 * the reader silently back to the first file, and with no visible list
 * position that reads as "the button is broken" rather than "you are at the
 * end". Returns null at either edge so the caller can disable the control.
 */
export function stepFile(
  rail: FileRail,
  currentPath: string | null,
  currentIsStaged: boolean,
  delta: number,
): RailEntry | null {
  if (rail.entries.length === 0 || delta === 0) return null;
  const index = rail.entries.findIndex((entry) =>
    isCurrent(entry, currentPath, currentIsStaged, rail.source),
  );
  // Nothing selected yet: a forward step opens the first file, a backward one
  // the last, so the controls do something sensible from a cold start.
  if (index < 0) return delta > 0 ? rail.entries[0] : rail.entries[rail.entries.length - 1];
  const next = index + delta;
  if (next < 0 || next >= rail.entries.length) return null;
  return rail.entries[next];
}

/** Position for the "3 of 12" readout; zeroes when nothing matches. */
export function railPosition(
  rail: FileRail,
  currentPath: string | null,
  currentIsStaged: boolean,
): { index: number; total: number } {
  const total = rail.entries.length;
  const found = rail.entries.findIndex((entry) =>
    isCurrent(entry, currentPath, currentIsStaged, rail.source),
  );
  return { index: found < 0 ? 0 : found + 1, total };
}

/**
 * What the rail's header says about completeness.
 *
 * Empty when the list is whole — the common case should add no chrome.
 */
export function truncationNote(rail: FileRail): string {
  if (!rail.truncated) return "";
  return rail.totalCount > 0
    ? `showing ${rail.entries.length} of ${rail.totalCount} files`
    : `showing the first ${rail.entries.length} files`;
}

/** `+12 −3`, or empty when a file records no line counts. */
export function churnLabel(entry: RailEntry): string {
  if (entry.additions === 0 && entry.deletions === 0) return "";
  return `+${entry.additions} −${entry.deletions}`;
}

/*
 * Row naming lives in `railRows` now, next to the disambiguation that decides
 * how much of a path a row has to show. A `displayName` here that answered
 * "the basename" was the reason two hundred rows could read `mod.rs`.
 */
