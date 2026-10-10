import { writable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { formatError } from "../ui/formatError";
import type { ProvenanceFreshness } from "./types";

/**
 * Freshness for the revisions currently on screen.
 *
 * Backed by one batched IPC call rather than one per row: the branch list and
 * the pull-request list each want a badge per row, and a per-row round trip
 * would be a `git rev-list` per branch. The backend resolves every revision in
 * a single `git cat-file`, reads each notes ref once, and measures only the
 * commits that actually carry a note.
 */

/** Upper bound on revisions asked for in one call. */
export const MAX_REVISIONS = 400;

export interface FreshnessState {
  /** Keyed by the revision as it was *requested*, so callers can look up by
   *  branch name or by sha without re-deriving anything. */
  byRevision: Record<string, ProvenanceFreshness>;
  loading: boolean;
  /**
   * Empty when the last load succeeded; otherwise why it failed.
   *
   * A failed load leaves `byRevision` as it was rather than clearing it: a
   * stale badge is a badge someone can still reason about, and an empty one
   * reads as "nothing is verified here", which would be a claim we cannot
   * make.
   */
  error: string;
  /**
   * True when the request was trimmed to `MAX_REVISIONS`.
   *
   * Rows past the cap simply have no entry, and no entry renders as no badge —
   * indistinguishable from a commit with nothing recorded. This is what lets a
   * caller say the difference out loud.
   */
  truncated: boolean;
}

const EMPTY: FreshnessState = {
  byRevision: {},
  loading: false,
  error: "",
  truncated: false,
};

export interface FreshnessStoreDeps {
  invoke?: typeof invoke;
}

export function createFreshnessStore(deps: FreshnessStoreDeps = {}) {
  const invokeFn = deps.invoke ?? invoke;
  const { subscribe, set, update } = writable<FreshnessState>({ ...EMPTY });

  /** The repository every answer held belongs to. */
  let repo: string | null = null;
  /** Moves on every repository change; an answer from before it is dropped. */
  let epoch = 0;
  /**
   * Monotonic request number. Loads overlap freely — the branch list and the
   * pull-request list each ask for their own rows, and either may ask again
   * after a refresh — and an older answer must never overwrite a newer one.
   */
  let token = 0;
  /** The newest request that asked about each revision. */
  let newest = new Map<string, number>();
  /** Requests still out, by what they asked, so an identical one joins. */
  let inflight = new Map<string, Promise<void>>();

  /**
   * Drops everything unless the answers held already belong to `repoPath`.
   *
   * Idempotent on purpose. The branch list called an unconditional reset from
   * an effect that re-ran on every repository-store emission, so each one
   * threw away whatever was in flight — its own load and the pull-request
   * list's — and the next emission asked again: ~54 batches in a launch's
   * first 30 s. A repeat for the same repository is now nothing, and a load
   * for another repository switches by itself, so neither order of the two
   * callers' effects can drop the other's answer.
   */
  function forRepo(repoPath: string | null) {
    if (repoPath === repo) return;
    repo = repoPath;
    epoch += 1;
    newest = new Map();
    inflight = new Map();
    set({ ...EMPTY });
  }

  /**
   * Measures `revisions` against `baseBranch`.
   *
   * Revisions may be shas or ref names; pull requests arrive as ref names.
   * Duplicates are collapsed before the call and expanded again on the way
   * back, so a branch that is also a PR head costs one measurement. A request
   * identical to one still out joins it rather than asking again.
   */
  function load(
    repoPath: string,
    revisions: readonly string[],
    baseBranch: string | null = null,
  ): Promise<void> {
    const unique = [...new Set(revisions.filter((r) => r.length > 0))];
    if (repoPath.length === 0 || unique.length === 0) return Promise.resolve();
    forRepo(repoPath);

    const truncated = unique.length > MAX_REVISIONS;
    const asked = truncated ? unique.slice(0, MAX_REVISIONS) : unique;
    const key = JSON.stringify([baseBranch, [...asked].sort()]);
    const joined = inflight.get(key);
    if (joined) return joined;

    const out = inflight;
    const run = request(repoPath, asked, baseBranch, truncated).finally(() => {
      if (out.get(key) === run) out.delete(key);
    });
    out.set(key, run);
    return run;
  }

  async function request(
    repoPath: string,
    asked: string[],
    baseBranch: string | null,
    truncated: boolean,
  ): Promise<void> {
    const mine = ++token;
    const mineEpoch = epoch;
    for (const rev of asked) newest.set(rev, mine);
    update((s) => ({ ...s, loading: true }));

    try {
      const rows = await invokeFn<ProvenanceFreshness[]>("cmd_provenance_freshness_batch", {
        repoPath,
        revisions: asked,
        baseBranch,
      });
      if (mineEpoch !== epoch) return;

      update((s) => {
        const loading = inflight.size > 1;
        // The backend answers one row per input, in input order — that is its
        // documented contract, and zipping is what makes a ref name usable as
        // a key. A short answer would silently mis-key every row after the
        // gap, so a mismatch is refused outright rather than half-applied.
        if (rows.length !== asked.length) {
          return {
            ...s,
            loading,
            error: `freshness: asked for ${asked.length} revisions and got ${rows.length}`,
          };
        }
        // Each row is kept unless a newer request has asked about the same
        // revision since: two lists' answers both land, and an older answer
        // for one revision never overwrites a newer one.
        const byRevision = { ...s.byRevision };
        asked.forEach((rev, i) => {
          if (newest.get(rev) === mine) byRevision[rev] = rows[i];
        });
        const latest = mine === token;
        return {
          byRevision,
          loading,
          error: latest ? "" : s.error,
          truncated: latest ? truncated : s.truncated,
        };
      });
    } catch (e) {
      if (mineEpoch !== epoch) return;
      update((s) => ({ ...s, loading: inflight.size > 1, error: formatError(e) }));
    }
  }

  return { subscribe, load, forRepo };
}

export const freshnessStore = createFreshnessStore();
