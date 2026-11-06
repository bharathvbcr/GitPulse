/**
 * What the graph says about a commit beyond its own row: the refs that point
 * at it, the merge its closing line descends to, and how many commits its
 * author has in the loaded history.
 *
 * These were derivations inline in the Graph view. The Diff view's history
 * rail draws the same graph and hovers the same card, so they live here once:
 * two copies would let the card say one thing in Graph and another in Diff
 * about the same commit.
 */
import { authorIdentity } from "../authors/authorIdentity";
import type { VisualCommitRow } from "../canvas/GraphRenderer";
import type { RefDecoration } from "../stores/graphStore";

export interface RefItem {
  name: string;
  kind: "head" | "current-branch" | "local-branch" | "remote-branch" | "tag" | "other";
  /**
   * For a tag: commits it holds that `comparedTo` does not have.
   *
   * A tag lane is shaped exactly like an unmerged branch, so the chip is
   * where the graph can say which it is. Both fields absent means the tag was
   * never measured (or is past the tag-list cap) — then the chip says nothing
   * rather than implying the work has landed.
   */
  aheadOfBase?: number | null;
  comparedTo?: string | null;
}

/** The fields of a tag listing this module reads (`TagInfo`, structurally). */
export interface TagStandingLike {
  name: string;
  commits_ahead_of_base: number;
  compared_to?: string | null;
}

/**
 * Ref chips per commit id.
 *
 * The tag listing the sidebar already loaded carries each tag's standing
 * against the default base. Reusing it costs the graph nothing: no extra
 * command, no extra git process on a path that runs on every repo switch. A
 * tag past the listing's cap is simply absent, and an absent tag makes no
 * claim rather than a false one.
 */
export function buildRefsByCommit(
  refs: readonly RefDecoration[],
  tags: readonly TagStandingLike[],
): Map<string, RefItem[]> {
  const standing = new Map<string, { ahead: number; base: string | null }>();
  for (const tag of tags) {
    standing.set(tag.name, { ahead: tag.commits_ahead_of_base, base: tag.compared_to ?? null });
  }
  const map = new Map<string, RefItem[]>();
  for (const ref of refs) {
    const kind: RefItem["kind"] =
      ref.kind === "head"
        ? "head"
        : ref.kind === "tag"
          ? "tag"
          : ref.kind === "remote"
            ? "remote-branch"
            // A ref outside branches, remotes and tags — only ever present
            // under the "all refs" scope. It gets its own chip rather than
            // falling through to "local-branch": drawing an agent-harness
            // checkpoint as a branch is the mislabelling this whole path
            // exists to prevent.
            : ref.kind === "other"
              ? "other"
              : ref.is_head
                ? "current-branch"
                : "local-branch";
    const list = map.get(ref.commit_id) ?? [];
    if (!list.some((r) => r.name === ref.name && r.kind === kind)) {
      const tag = kind === "tag" ? standing.get(ref.name) : undefined;
      list.push({
        name: ref.name,
        kind,
        aheadOfBase: tag?.ahead ?? null,
        comparedTo: tag?.base ?? null,
      });
    }
    map.set(ref.commit_id, list);
  }
  return map;
}

/**
 * Merge destination per closing commit: the parent a commit's first-parent
 * edge lands on when it leaves its own column. This is the relationship the
 * graph draws as a descending connector; rows carry it as screen-reader text
 * and both tooltip modes name it, so the information is never pointer-only.
 */
export function buildCloseTargets(rows: readonly VisualCommitRow[]): Map<string, VisualCommitRow> {
  const map = new Map<string, VisualCommitRow>();
  for (let i = 0; i < rows.length; i++) {
    const first = rows[i].connections?.[0];
    if (!first || first.is_dangling || first.is_merge) continue;
    if (!Number.isFinite(first.to_lane) || first.to_lane === first.from_lane) continue;
    const target = i + first.to_row_offset;
    if (!Number.isFinite(target) || target <= i || target >= rows.length) continue;
    map.set(rows[i].id, rows[target]);
  }
  return map;
}

/**
 * Commits per author identity across `rows`, keyed by the canonical identity
 * key (email first, then name) so display-name changes do not split one
 * person's count.
 */
export function countCommitsByAuthor(rows: readonly VisualCommitRow[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const row of rows) {
    const key = authorIdentity(row.author_name, row.author_email).key;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}

/** `row`'s author's count in `counts`, or null when the author is not in it. */
export function authorCountFor(
  counts: ReadonlyMap<string, number>,
  row: Pick<VisualCommitRow, "author_name" | "author_email">,
): number | null {
  return counts.get(authorIdentity(row.author_name, row.author_email).key) ?? null;
}
