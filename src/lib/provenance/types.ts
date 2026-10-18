/**
 * Wire types for git-native provenance notes.
 *
 * A verification note is a claim about a tree at a moment; freshness says how
 * far the world has moved since. These live in their own module because
 * `wire-type-locality-contract` forbids snake_case payload shapes inside
 * `.svelte` files, where `check:types` cannot reach them.
 *
 * Mirrors `src-tauri/src/engine/provenance.rs`.
 */

/** A recorded verification of one commit. */
export interface VerificationNote {
  verdict: string;
  verified_at: number;
  checked_by: string;
  task_id: string | null;
  details: string | null;
}

/** One agent session episode, attached to the commit it produced. */
export interface SessionEpisodeNote {
  session_id: string;
  actor_kind: string;
  transcript_path: string | null;
  created_at: number;
  summary: string | null;
}

/** How far the base has moved since a commit was noted. */
export interface ProvenanceFreshness {
  commit_sha: string;
  /**
   * How many commits the base has that this one does not
   * (`rev-list --count <commit>..<base>`), or `null` when it could not be
   * measured. It is 0 for a commit *ahead of* the base as well as for the base
   * tip, so it says how far the base moved, not whether the commit is on it:
   * read `landed_on_base` and `ahead` for that.
   *
   * `null` is not zero. Zero is the strongest claim this type can make —
   * "nothing has moved since this was verified" — so a failed measurement
   * must never arrive as one.
   */
  distance: number | null;
  /** Decays with distance. `null` when distance could not be measured. */
  confidence: number | null;
  /** True only when the distance was measured *and* is zero. */
  is_fresh: boolean;
  /** Empty when the distance was measured; otherwise why it was not. */
  unmeasured_reason: string;
  /**
   * Whether the note refs could be read at all.
   *
   * False means we do not know whether this commit is verified — which is a
   * different thing from knowing that it is not. Without it, "this repository
   * has never recorded a verification" and "its notes could not be read"
   * arrive as the same empty answer, and the badge would have to render an
   * unexamined commit as an unverified one.
   */
  notes_readable: boolean;
  verification: VerificationNote | null;
  session: SessionEpisodeNote | null;
  /**
   * Whether the commit is reachable from the base (landed there by fast-forward
   * or merge). `false` is a measured "not reachable": a commit *ahead of* the
   * base has `distance` 0 and is still `false` here. `null` when it could not
   * be measured (unresolvable base, shallow clone, a badge row) — never `false`.
   */
  landed_on_base: boolean | null;
  /** The same reachability, under the name the freshness fields are read against. */
  on_base: boolean | null;
  /** The commit the base resolved to. */
  base_oid: string | null;
  /** The common ancestor of the commit and the base. */
  merge_base: string | null;
  /** Commits on this one and not on the base. */
  ahead: number | null;
  /** Commits on the base and not on this one; equals `distance` when both were measured. */
  behind: number | null;
  /** Empty when the relation was measured; otherwise why it was not. */
  base_relation_reason: string;
}
