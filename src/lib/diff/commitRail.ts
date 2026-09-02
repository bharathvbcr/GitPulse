/**
 * The commit picker that sits above the file list in the Diff view.
 *
 * The file rail removed the round trip *within* one commit. This removes the
 * one *between* commits: comparing how a file changed across two commits still
 * meant leaving for the Graph view, finding the next commit, clicking its
 * file, and being switched back — the same nine-round-trip shape, one level up.
 *
 * Like the file rail, it is pure and costs no IPC: `graphStore.rows` already
 * holds every commit the graph drew, so this picker can never show a commit
 * the graph does not, or miss one it does.
 *
 * Kept separate from `fileRail` deliberately. They render next to each other
 * but answer different questions — "which change am I looking at" versus
 * "which file within it" — and a single module juggling both would need a
 * mode flag threaded through every function.
 */

/** Structural shape of `VisualCommitRow`, so this module imports no canvas code. */
export interface CommitRowLike {
  id: string;
  summary: string;
  author_name: string;
  timestamp: number;
  is_merge: boolean;
}

export interface CommitEntry {
  id: string;
  summary: string;
  authorName: string;
  timestamp: number;
  isMerge: boolean;
}

export interface CommitRail {
  entries: CommitEntry[];
  /** True when more commits exist than the rail is showing. */
  truncated: boolean;
  /** How many commits the graph actually holds, when more than are shown. */
  totalCount: number;
}

/**
 * How many commits the picker renders.
 *
 * The graph can hold tens of thousands of rows and this list is not a history
 * browser — it is a shortcut to the handful you are moving between. Rendering
 * them all would put a multi-thousand-row list in a 224px rail and make the
 * view janky to open, for a case the Graph view already serves better.
 */
export const MAX_PICKER_COMMITS = 40;

export const EMPTY_COMMIT_RAIL: CommitRail = {
  entries: [],
  truncated: false,
  totalCount: 0,
};

/**
 * Builds the picker from the rows the graph already drew.
 *
 * Order is preserved exactly as the graph produced it — newest first, with
 * whatever filter or revision the user set still applied. Re-sorting here
 * would make the picker disagree with the graph beside it, and the reader has
 * no way to tell which one is lying.
 */
export function buildCommitRail(
  rows: readonly CommitRowLike[] | null | undefined,
  limit: number = MAX_PICKER_COMMITS,
): CommitRail {
  if (!rows || rows.length === 0) return EMPTY_COMMIT_RAIL;
  const capped = limit > 0 ? rows.slice(0, limit) : [];
  return {
    entries: capped.map((row) => ({
      id: row.id,
      summary: row.summary,
      authorName: row.author_name,
      timestamp: row.timestamp,
      isMerge: row.is_merge,
    })),
    truncated: rows.length > capped.length,
    totalCount: rows.length > capped.length ? rows.length : 0,
  };
}

/**
 * What the picker says when it is not showing everything.
 *
 * The Graph view is named as the place that does, so the note is a route
 * rather than only an apology.
 */
export function pickerNote(rail: CommitRail): string {
  if (!rail.truncated) return "";
  return rail.totalCount > 0
    ? `newest ${rail.entries.length} of ${rail.totalCount} — see Graph for the rest`
    : `newest ${rail.entries.length} — see Graph for the rest`;
}

/**
 * A commit's one-line label.
 *
 * An empty summary is real — `git commit --allow-empty-message` produces one —
 * and rendering a blank row would look like a loading failure.
 */
export function commitLabel(entry: CommitEntry): string {
  const summary = entry.summary.trim();
  return summary.length > 0 ? summary : "(no commit message)";
}

/**
 * Whether this entry is the change currently on screen.
 *
 * Compared by prefix in either direction so an abbreviated id from one source
 * still matches a full one from another; both are hex ids of the same commit,
 * and a strict equality check would leave the list with nothing highlighted.
 */
export function isCurrentCommit(entry: CommitEntry, selectedCommitId: string | null): boolean {
  if (!selectedCommitId) return false;
  const a = entry.id;
  const b = selectedCommitId;
  if (a === b) return true;
  const shorter = a.length < b.length ? a : b;
  // A one-character "prefix" matches almost everything; require enough of an
  // id that the match means something.
  if (shorter.length < 7) return false;
  return a.startsWith(shorter) && b.startsWith(shorter);
}
