/**
 * Picking the local commit a pull request's head names.
 *
 * A pull request carries a branch name, not a sha, and that branch usually
 * exists here only under a remote — `origin/feature/x`, not `feature/x`. Git's
 * own revision lookup will not bridge that gap, so a bare head ref resolves to
 * nothing for almost every PR that is not also checked out locally.
 *
 * So each PR is asked about under a small list of candidate revisions and the
 * first one that resolved wins. A PR whose branch lives under a remote named
 * something other than `origin`, or that was never fetched, resolves under
 * none of them — and reads as *unknown*, which is what it is. It must not read
 * as unverified: we did not look at that commit and find nothing, we never
 * found the commit.
 */

import type { ProvenanceFreshness } from "./types";

/** Remote checked before the bare name. */
export const PRIMARY_REMOTE = "origin";

/** A resolved answer carries the commit it resolved to; an unresolved one
 *  carries back the revision it was asked about. */
const SHA = /^[0-9a-f]{40}$/;

/** True when the backend found a commit for this revision. */
export function wasResolved(f: ProvenanceFreshness): boolean {
  return SHA.test(f.commit_sha);
}

/**
 * Revisions worth asking about for one head ref, most specific first.
 *
 * A head ref that is already a sha is asked about as itself and nothing else:
 * prefixing a remote onto a sha would produce a revision that cannot exist.
 */
export function prRevisionCandidates(headRef: string): string[] {
  const ref = headRef.trim();
  if (ref.length === 0) return [];
  if (SHA.test(ref)) return [ref];
  return [`${PRIMARY_REMOTE}/${ref}`, ref];
}

/** A pull request whose head branch is in a fork. Its `head_ref` names a
 *  branch of the contributor's repository, so it says nothing about the
 *  same-named local or `origin/` branch (a fork's `main` is not our `main`). */
export function isForkPr(pr: { is_cross_repository?: boolean }): boolean {
  return pr.is_cross_repository === true;
}

/** Every revision a batch should carry for these pull requests, deduplicated.
 *  Fork heads are skipped: asking about `origin/<their branch>` would answer
 *  for an unrelated branch of ours. */
export function prRevisions(
  prs: readonly { head_ref: string; is_cross_repository?: boolean }[],
): string[] {
  return [
    ...new Set(prs.filter((pr) => !isForkPr(pr)).flatMap((pr) => prRevisionCandidates(pr.head_ref))),
  ];
}

/**
 * The freshness to show for one pull request.
 *
 * Returns the first candidate that resolved to a commit. When none did, the
 * *last* candidate's answer is returned rather than null, so the row still
 * renders "unknown" with git's own reason attached — a silent absence would be
 * indistinguishable from a PR whose head is verified-clean.
 */
export function prFreshness(
  byRevision: Readonly<Record<string, ProvenanceFreshness>>,
  headRef: string,
): ProvenanceFreshness | null {
  const candidates = prRevisionCandidates(headRef);
  let lastSeen: ProvenanceFreshness | null = null;
  for (const rev of candidates) {
    const answer = byRevision[rev];
    if (!answer) continue;
    if (wasResolved(answer)) return answer;
    lastSeen = answer;
  }
  return lastSeen;
}

/** The freshness badge value for a pull request: none for a fork head. */
export function prHeadFreshness(
  byRevision: Readonly<Record<string, ProvenanceFreshness>>,
  pr: { head_ref: string; is_cross_repository?: boolean },
): ProvenanceFreshness | null {
  return isForkPr(pr) ? null : prFreshness(byRevision, pr.head_ref);
}
