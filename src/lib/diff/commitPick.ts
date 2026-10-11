import type { CommitFileChange } from "../stores/graphStore";
import { formatError } from "../ui/formatError";

/**
 * What reading one commit's file list came to. A failed read is its own
 * state: it used to be stored as an empty list, which the rail rendered as "a
 * commit that touched one file" and which was then cached for good.
 */
export type CommitFilesRead =
  | { commitId: string; files: CommitFileChange[]; error: null }
  | { commitId: string; files: null; error: string };

export type CommitPickDeps = {
  /** The repository on screen now; a pick that started under another one is dropped. */
  repo: () => string | null;
  listFiles: (repo: string, commitId: string) => Promise<CommitFileChange[]>;
  /** A successful read already held for this commit, if any. */
  cached: (commitId: string) => CommitFileChange[] | null;
  remember: (read: CommitFilesRead) => void;
  openFile: (commitId: string, path: string) => void;
  openCommit: (commitId: string) => void;
};

export type CommitPickOutcome = "opened" | "stale" | "failed";

/**
 * Switches the diff to another commit and opens its first file.
 *
 * Only the latest pick may act: quick picks of A then B must end on B even
 * when A's file list is the slower answer, and an answer that lands after a
 * repository switch must not open a commit of the previous repository. A
 * failed list opens nothing and is recorded as an error the viewer shows with
 * a retry, instead of being read as "this commit has no files".
 */
export function createCommitPicker(deps: CommitPickDeps) {
  let latest = 0;
  return async function pick(commitId: string): Promise<CommitPickOutcome> {
    const repo = deps.repo();
    if (!repo) return "stale";
    const token = ++latest;
    let files = deps.cached(commitId);
    if (!files) {
      try {
        files = await deps.listFiles(repo, commitId);
      } catch (error) {
        if (token !== latest || deps.repo() !== repo) return "stale";
        deps.remember({ commitId, files: null, error: formatError(error) });
        return "failed";
      }
      if (token !== latest || deps.repo() !== repo) return "stale";
      deps.remember({ commitId, files, error: null });
    }
    const first = files[0];
    // A commit that changed nothing (an empty or a merge with no diff) has no
    // file to open; show the whole commit rather than nothing at all.
    if (first) deps.openFile(commitId, first.path);
    else deps.openCommit(commitId);
    return "opened";
  };
}
