import { describe, expect, it } from "vitest";
import { prFreshness, prRevisionCandidates, prRevisions, wasResolved } from "./pullRequests";
import { freshnessBadge } from "./badge";
import type { ProvenanceFreshness } from "./types";

const SHA_A = "a".repeat(40);
const SHA_B = "b".repeat(40);

function resolved(sha: string, over: Partial<ProvenanceFreshness> = {}): ProvenanceFreshness {
  return {
    commit_sha: sha,
    distance: 0,
    confidence: 1,
    is_fresh: true,
    unmeasured_reason: "",
    notes_readable: true,
    verification: null,
    session: null,
    ...over,
  };
}

function unresolved(rev: string): ProvenanceFreshness {
  return {
    commit_sha: rev,
    distance: null,
    confidence: null,
    is_fresh: false,
    unmeasured_reason: `${rev} does not name a commit in this repository`,
    notes_readable: false,
    verification: null,
    session: null,
  };
}

describe("prRevisionCandidates", () => {
  it("prefers the remote-tracking ref, because that is where a PR branch lives", () => {
    expect(prRevisionCandidates("feature/x")).toEqual(["origin/feature/x", "feature/x"]);
  });

  it("asks about a sha as itself, never under a remote", () => {
    // `origin/<sha>` is a revision that cannot exist; asking costs a slot in
    // the batch and can only ever answer "not found".
    expect(prRevisionCandidates(SHA_A)).toEqual([SHA_A]);
  });

  it("has nothing to ask about for an empty head ref", () => {
    expect(prRevisionCandidates("")).toEqual([]);
    expect(prRevisionCandidates("   ")).toEqual([]);
  });
});

describe("prRevisions", () => {
  it("deduplicates across pull requests sharing a head", () => {
    const revs = prRevisions([
      { head_ref: "feature/x" },
      { head_ref: "feature/x" },
      { head_ref: "feature/y" },
    ]);
    expect(revs).toEqual([
      "origin/feature/x",
      "feature/x",
      "origin/feature/y",
      "feature/y",
    ]);
  });
});

describe("prFreshness", () => {
  it("takes the remote-tracking answer when it resolved", () => {
    const state = {
      "origin/feature/x": resolved(SHA_A),
      "feature/x": resolved(SHA_B),
    };
    expect(prFreshness(state, "feature/x")?.commit_sha).toBe(SHA_A);
  });

  it("falls back to the local branch when the remote ref is not here", () => {
    const state = {
      "origin/feature/x": unresolved("origin/feature/x"),
      "feature/x": resolved(SHA_B),
    };
    expect(prFreshness(state, "feature/x")?.commit_sha).toBe(SHA_B);
  });

  /**
   * The distinction the whole module exists for: a PR head that is not in this
   * checkout must read as *unknown*, never as a clean unverified row. We did
   * not look at that commit — we never found it.
   */
  it("reports unknown, not absence, when nothing resolved", () => {
    const state = {
      "origin/feature/x": unresolved("origin/feature/x"),
      "feature/x": unresolved("feature/x"),
    };
    const f = prFreshness(state, "feature/x");
    expect(f).not.toBeNull();
    expect(wasResolved(f!)).toBe(false);

    const badge = freshnessBadge(f!);
    expect(badge.kind).toBe("unknown");
    expect(badge.detail).toContain("does not name a commit");
  });

  it("is null only while the answer has not arrived", () => {
    expect(prFreshness({}, "feature/x")).toBeNull();
  });
});

describe("wasResolved", () => {
  it("is true exactly for a 40-character sha", () => {
    expect(wasResolved(resolved(SHA_A))).toBe(true);
    expect(wasResolved(unresolved("origin/main"))).toBe(false);
    expect(wasResolved(unresolved("a".repeat(39)))).toBe(false);
    expect(wasResolved(unresolved("A".repeat(40)))).toBe(false);
  });
});
