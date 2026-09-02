import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
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

  /**
   * Monotonic token. Loads overlap freely — a repo switch, a branch refresh
   * and a PR refresh can all be in flight — and an older answer must never
   * overwrite a newer one.
   */
  let token = 0;

  /** Drops everything. Called when the repository changes. */
  function reset() {
    token += 1;
    set({ ...EMPTY });
  }

  /**
   * Measures `revisions` against `baseBranch`.
   *
   * Revisions may be shas or ref names; pull requests arrive as ref names.
   * Duplicates are collapsed before the call and expanded again on the way
   * back, so a branch that is also a PR head costs one measurement.
   */
  async function load(
    repoPath: string,
    revisions: readonly string[],
    baseBranch: string | null = null,
  ): Promise<void> {
    const unique = [...new Set(revisions.filter((r) => r.length > 0))];
    if (repoPath.length === 0 || unique.length === 0) return;

    const truncated = unique.length > MAX_REVISIONS;
    const asked = truncated ? unique.slice(0, MAX_REVISIONS) : unique;

    const mine = ++token;
    update((s) => ({ ...s, loading: true }));

    try {
      const rows = await invokeFn<ProvenanceFreshness[]>("cmd_provenance_freshness_batch", {
        repoPath,
        revisions: asked,
        baseBranch,
      });
      if (mine !== token) return;

      update((s) => {
        const byRevision = { ...s.byRevision };
        // The backend answers one row per input, in input order — that is its
        // documented contract, and zipping is what makes a ref name usable as
        // a key. A short answer would silently mis-key every row after the
        // gap, so a mismatch is refused outright rather than half-applied.
        if (rows.length !== asked.length) {
          return {
            ...s,
            loading: false,
            error: `freshness: asked for ${asked.length} revisions and got ${rows.length}`,
          };
        }
        asked.forEach((rev, i) => {
          byRevision[rev] = rows[i];
        });
        return { byRevision, loading: false, error: "", truncated };
      });
    } catch (e) {
      if (mine !== token) return;
      update((s) => ({ ...s, loading: false, error: formatError(e) }));
    }
  }

  return { subscribe, load, reset };
}

export const freshnessStore = createFreshnessStore();
