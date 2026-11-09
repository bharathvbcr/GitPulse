import { get } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { currentHeadTip } from "../stores/headTip";
import { currentRepoPath, repoStore } from "../stores/repoStore";
import { statusPathKey } from "./fileStatus";

/**
 * The repository file listing (`cmd_list_repo_files`), asked once and shared.
 *
 * The Code view and the command palette both want the same list, which on a
 * 100k-file repository is a 100k-string IPC decode on the main thread. Each
 * used to ask on its own, the Code view on every entry. One owner answers
 * both: a listing is reused while nothing that could change it has changed.
 *
 * What changes a listing is what the Code view already re-lists on: the set of
 * changed paths (an untracked file appears there), HEAD (a checkout, pull or
 * commit), and a full hydrate of the repository (`contentRevisions`). The key
 * is built from those; a different key is a miss.
 *
 * Only the repository on screen is cached, because only it has that key. A
 * request for any other repository goes straight to the backend. A failed
 * read is never kept, so an error cannot be served back as a listing, and a
 * caller that wants a fresh read (Retry) says `force`.
 */

/** Listings kept at once; each can be 100k strings. */
export const MAX_CACHED_LISTINGS = 4;

export interface RepoListingDeps {
  fetch: (repoPath: string) => Promise<readonly string[]>;
  /** What the listing of this repository depends on, or null if it cannot be keyed. */
  keyFor: (repoPath: string) => string | null;
}

interface Entry {
  key: string;
  promise: Promise<readonly string[]>;
  list: readonly string[] | null;
}

export function createRepoListing(deps: RepoListingDeps) {
  const entries = new Map<string, Entry>();

  function current(repoPath: string): Entry | null {
    const key = deps.keyFor(repoPath);
    const entry = entries.get(repoPath);
    return key !== null && entry && entry.key === key ? entry : null;
  }

  return {
    /** The listing if one is held for the repository's current key, else null. Never fetches. */
    peek(repoPath: string): readonly string[] | null {
      return current(repoPath)?.list ?? null;
    },

    /** The listing: held, joined while it is being read, or read now. */
    list(repoPath: string, options: { force?: boolean } = {}): Promise<readonly string[]> {
      const held = options.force ? null : current(repoPath);
      if (held) return held.promise;
      const key = deps.keyFor(repoPath);
      if (key === null) return deps.fetch(repoPath);
      const entry: Entry = { key, list: null, promise: deps.fetch(repoPath) };
      entry.promise = entry.promise.then(
        (list) => {
          entry.list = list;
          return list;
        },
        (error: unknown) => {
          if (entries.get(repoPath) === entry) entries.delete(repoPath);
          throw error;
        },
      );
      entries.delete(repoPath);
      entries.set(repoPath, entry);
      while (entries.size > MAX_CACHED_LISTINGS) {
        const oldest = entries.keys().next();
        if (oldest.done) break;
        entries.delete(oldest.value);
      }
      return entry.promise;
    },

    /** Forget one repository's listing, or all of them. */
    invalidate(repoPath?: string): void {
      if (repoPath === undefined) entries.clear();
      else entries.delete(repoPath);
    },
  };
}

const repoListing = createRepoListing({
  fetch: (repoPath) => invoke<string[]>("cmd_list_repo_files", { repoPath }),
  keyFor: (repoPath) => {
    if (get(currentRepoPath) !== repoPath) return null;
    return [
      get(currentHeadTip),
      get(repoStore.contentRevisions)[repoPath] ?? "",
      statusPathKey(get(repoStore).statuses),
    ].join("\u0000");
  },
});

export const listRepoFiles = repoListing.list;
export const peekRepoListing = repoListing.peek;
export const invalidateRepoListing = repoListing.invalidate;
