import { describe, expect, it } from "vitest";
import { freshnessBadge, isNoteworthy, NARROWED_VERDICTS, PASS_VERDICTS } from "./badge";
import type { ProvenanceFreshness, VerificationNote } from "./types";

function note(verdict: string): VerificationNote {
  return { verdict, verified_at: 1_788_000_000, checked_by: "ci.local", task_id: null, details: null };
}

function f(over: Partial<ProvenanceFreshness> = {}): ProvenanceFreshness {
  return {
    commit_sha: "a".repeat(40),
    distance: 0,
    confidence: 1,
    is_fresh: true,
    unmeasured_reason: "",
    notes_readable: true,
    verified: true,
    verification: null,
    session: null,
    session_basis: "",
    session_ambiguous: false,
    landed_on_base: null,
    on_base: null,
    base_oid: null,
    merge_base: null,
    ahead: null,
    behind: null,
    base_relation_reason: "not measured",
    ...over,
  };
}

describe("narrowed verdict adversarial", () => {
  it("is never good, at distance 0, null, or large", () => {
    for (const distance of [0, null, 1, 10_000]) {
      const b = freshnessBadge(f({ verification: note("passed-affected"), distance }));
      expect(b.kind).toBe("narrowed");
      expect(b.tone).toBe("warn");
      expect(b.label).not.toBe("verified");
    }
  });

  it("is read case- and whitespace-insensitively, like a pass", () => {
    for (const v of ["Passed-Affected", "  passed-affected\n", "PASSED-AFFECTED"]) {
      expect(freshnessBadge(f({ verification: note(v) })).kind).toBe("narrowed");
    }
  });

  it("near-miss spellings are unrecognised, not narrowed and not passes", () => {
    for (const v of ["passed_affected", "passed affected", "affected-passed", "passed-affected2", "passed-"]) {
      expect(freshnessBadge(f({ verification: note(v) })).kind).toBe("unrecognised");
    }
  });

  it("is in neither the pass set nor the fail set and is noteworthy", () => {
    expect(PASS_VERDICTS).not.toContain("passed-affected");
    expect(NARROWED_VERDICTS).toEqual(["passed-affected"]);
    expect(isNoteworthy(freshnessBadge(f({ verification: note("passed-affected") })))).toBe(true);
  });

  it("unreadable notes still outrank a narrowed verdict", () => {
    expect(freshnessBadge(f({ notes_readable: false, verification: note("passed-affected") })).kind).toBe("unknown");
  });

  it("does not trust is_fresh/confidence to upgrade it", () => {
    const b = freshnessBadge(f({ verification: note("passed-affected"), is_fresh: true, confidence: 1 }));
    expect(b.kind).toBe("narrowed");
  });

  it("states that the rest of the suite did not run", () => {
    const b = freshnessBadge(f({ verification: note("passed-affected"), distance: 2 }));
    expect(b.detail).toMatch(/rest of the suite was not run/);
    expect(b.detail).toMatch(/2 commits/);
  });

  it("verification present with an empty verdict is unrecognised, never a pass", () => {
    expect(freshnessBadge(f({ verification: note("") })).kind).toBe("unrecognised");
  });
});
