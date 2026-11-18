import type { CommitGraphPayload } from "../stores/graphStore";
import { formatError } from "../ui/formatError";

/**
 * The commits that changed one file, newest first: the read behind Live
 * pulse's "Recent file history" and the open file's History menu.
 *
 * It is the commit graph limited to the file (`path:` in the graph query),
 * walked from HEAD. Without a revision the graph walks HEAD plus every branch,
 * remote branch and tag (`graph/ref_scope.rs`), which listed commits from
 * branches that were never merged here as this file's past: on this
 * repository, README.md's "newest change" was a commit only on origin/main.
 * The backend hands the path to `git log` as a literal pathspec, so `[`, `*`
 * and `?` in a file name match only themselves. Two limits are stated rather
 * than hidden:
 *
 *  - The query grammar splits on whitespace and has no quoting, so a path
 *    containing whitespace cannot be expressed. Its second half would become a
 *    message-text search and the answer would be wrong, so such a path reads
 *    as `unsupported`, not as a history with no commits.
 *  - History is not followed across renames (`git log` without `--follow`):
 *    it ends at the commit that gave the file its current name.
 *
 * A failed read is `error`, never an empty list: "no commits touch this file"
 * and "the history could not be read" must not look the same.
 */
export interface FileHistoryCommit {
  id: string;
  author: string;
  /** Commit time in milliseconds. */
  date: number;
  message: string;
}

export type FileHistory =
  | { status: "loaded"; commits: FileHistoryCommit[]; hasMore: boolean }
  | { status: "unsupported"; reason: string }
  | { status: "error"; message: string };

/** Where the walk starts: the checked-out history, which is the open file's past. */
export const FILE_HISTORY_REVISION = "HEAD";

/** Most commits one read will ask for. */
export const MAX_FILE_HISTORY = 50;

export const WHITESPACE_PATH_REASON =
  "History can't be listed for a path that contains spaces yet.";

/** The graph query for `path`'s history, or null when the grammar cannot express it. */
export function fileHistoryQuery(path: string): string | null {
  if (!path || /\s/.test(path)) return null;
  return `path:${path}`;
}

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * Reads up to `limit` of the newest commits that changed `path`. `hasMore`
 * says the backend found older ones past the limit, so a caller can say the
 * list is the newest N rather than all of them.
 */
export async function loadFileHistory(
  invoke: Invoke,
  repoPath: string,
  path: string,
  limit: number,
): Promise<FileHistory> {
  const query = fileHistoryQuery(path);
  if (query === null) return { status: "unsupported", reason: WHITESPACE_PATH_REASON };
  const cap = Math.max(1, Math.min(MAX_FILE_HISTORY, Math.trunc(limit) || 1));
  try {
    const graph = await invoke<CommitGraphPayload>("cmd_get_commit_graph", {
      repoPath,
      maxCommits: cap,
      query,
      revision: FILE_HISTORY_REVISION,
      skip: 0,
    });
    const rows = Array.isArray(graph?.rows) ? graph.rows : [];
    return {
      status: "loaded",
      commits: rows.slice(0, cap).map((row) => ({
        id: row.id,
        author: row.author_name,
        date: row.timestamp * 1000,
        message: row.summary,
      })),
      hasMore: rows.length > cap || graph?.has_more === true,
    };
  } catch (error) {
    return { status: "error", message: formatError(error) };
  }
}
