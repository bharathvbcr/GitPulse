import { describe, expect, it } from "vitest";
import {
  confidencePercent,
  FAIL_VERDICTS,
  freshnessBadge,
  isNoteworthy,
  PASS_VERDICTS,
  type FreshnessKind,
} from "./badge";
import type { ProvenanceFreshness, SessionEpisodeNote, VerificationNote } from "./types";

/**
 * The complete set of kinds, spelled out once so the reachability test below
 * can be a real check rather than a restatement of whatever the classifier
 * happens to return. TypeScript rejects this literal if `FreshnessKind` gains
 * or loses a member, so the list cannot drift.
 */
const ALL_KINDS: readonly FreshnessKind[] = [
  "unknown",
  "failed",
  "unrecognised",
  "unmeasured",
  "fresh",
  "stale",
  "agent",
  "unverified",
];

function note(verdict: string): VerificationNote {
  return {
    verdict,
    verified_at: 1_788_000_000,
    checked_by: "ci.local",
    task_id: "TASK-1",
    details: "Tests=passed",
  };
}

function session(): SessionEpisodeNote {
  return {
    session_id: "S1",
    actor_kind: "agent",
    transcript_path: null,
    created_at: 1_788_000_000,
    summary: "wrote the batch path",
  };
}

function fresh(over: Partial<ProvenanceFreshness> = {}): ProvenanceFreshness {
  return {
    commit_sha: "a".repeat(40),
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

describe("freshnessBadge", () => {
  it("reaches every kind it declares", () => {
    // A kind nothing can produce is dead vocabulary, and a kind the classifier
    // produces that nobody styled is an unlabelled badge. Both are caught here.
    const reached = new Set<FreshnessKind>([
      freshnessBadge(fresh({ notes_readable: false })).kind,
      freshnessBadge(fresh({ verification: note("failed") })).kind,
      freshnessBadge(fresh({ verification: note("pending-review") })).kind,
      freshnessBadge(
        fresh({ verification: note("passed"), distance: null, confidence: null, is_fresh: false }),
      ).kind,
      freshnessBadge(fresh({ verification: note("passed") })).kind,
      freshnessBadge(
        fresh({ verification: note("passed"), distance: 3, confidence: 0.769, is_fresh: false }),
      ).kind,
      freshnessBadge(fresh({ session: session() })).kind,
      freshnessBadge(fresh()).kind,
    ]);
    expect([...reached].sort()).toEqual([...ALL_KINDS].sort());
  });

  describe("the honesty invariant", () => {
    it("never calls an unverified commit verified, however fresh it looks", () => {
      // `is_fresh: true` and `distance: 0` are the strongest signals in the
      // payload, and neither of them says anything was ever checked.
      const b = freshnessBadge(fresh({ distance: 0, confidence: 1, is_fresh: true }));
      expect(b.kind).toBe("unverified");
      expect(b.tone).not.toBe("good");
      expect(b.label).not.toContain("verified · ");
    });

    it("distinguishes notes we could not read from notes that do not exist", () => {
      const missing = freshnessBadge(fresh({ unmeasured_reason: "no provenance note" }));
      const unreadable = freshnessBadge(
        fresh({
          notes_readable: false,
          distance: null,
          confidence: null,
          is_fresh: false,
          unmeasured_reason: "git notes list failed: bad object",
        }),
      );
      expect(missing.kind).toBe("unverified");
      expect(unreadable.kind).toBe("unknown");
      expect(unreadable.detail).toContain("bad object");
    });

    it("does not let an unreadable ledger hide behind a verification it also carries", () => {
      // Ordering: `unknown` is tested before anything else, so a payload that
      // somehow carries both an unreadable flag and a note still reports that
      // we could not establish the state.
      const b = freshnessBadge(
        fresh({ notes_readable: false, verification: note("passed"), distance: 0 }),
      );
      expect(b.kind).toBe("unknown");
    });

    it("treats a verdict it does not recognise as not-a-pass", () => {
      const b = freshnessBadge(fresh({ verification: note("needs-attention") }));
      expect(b.kind).toBe("unrecognised");
      expect(b.tone).not.toBe("good");
      expect(b.detail).toContain("needs-attention");
    });

    it("never reports an unmeasured distance as fresh", () => {
      const b = freshnessBadge(
        fresh({
          verification: note("passed"),
          distance: null,
          confidence: null,
          is_fresh: false,
          unmeasured_reason: "git rev-list a..main failed",
        }),
      );
      expect(b.kind).toBe("unmeasured");
      expect(b.tone).toBe("warn");
      expect(b.detail).toContain("git rev-list");
    });

    it("keeps a recorded failure a failure however stale it is", () => {
      for (const distance of [0, 1, 900, null]) {
        const b = freshnessBadge(
          fresh({ verification: note("failed"), distance, is_fresh: distance === 0 }),
        );
        expect(b.kind, `distance ${distance}`).toBe("failed");
        expect(b.tone).toBe("bad");
      }
    });
  });

  it("recognises every pass and fail word it publishes, in any case", () => {
    // The two vocabularies are exported so a contract test can compare them
    // against the Rust writer. Exporting a word the classifier does not
    // actually honour would make that comparison meaningless.
    for (const verdict of PASS_VERDICTS) {
      expect(freshnessBadge(fresh({ verification: note(verdict) })).kind).toBe("fresh");
      expect(freshnessBadge(fresh({ verification: note(verdict.toUpperCase()) })).kind).toBe(
        "fresh",
      );
    }
    for (const verdict of FAIL_VERDICTS) {
      expect(freshnessBadge(fresh({ verification: note(verdict) })).kind).toBe("failed");
      expect(freshnessBadge(fresh({ verification: note(` ${verdict} `) })).kind).toBe("failed");
    }
  });

  it("the two vocabularies do not overlap", () => {
    // An overlap would make the classification depend on which check runs
    // first rather than on what the verdict says.
    const both = PASS_VERDICTS.filter((v) => FAIL_VERDICTS.includes(v));
    expect(both).toEqual([]);
  });

  it("names the distance and the decayed confidence when it is stale", () => {
    const b = freshnessBadge(
      fresh({ verification: note("passed"), distance: 3, confidence: 0.769, is_fresh: false }),
    );
    expect(b.kind).toBe("stale");
    expect(b.label).toBe("verified · 3 behind");
    expect(b.detail).toContain("3 commits");
    expect(b.detail).toContain("77%");
    expect(b.detail).toContain("ci.local");
  });

  it("says one commit rather than 1 commits", () => {
    const b = freshnessBadge(
      fresh({ verification: note("passed"), distance: 1, confidence: 0.909, is_fresh: false }),
    );
    expect(b.detail).toContain("1 commit since");
  });

  it("credits an agent session without calling it a verification", () => {
    const b = freshnessBadge(fresh({ session: session() }));
    expect(b.kind).toBe("agent");
    expect(b.label).toContain("unverified");
    expect(b.detail).toContain("S1");
    expect(b.detail).toContain("wrote the batch path");
  });

  it("omits the checker when the note does not name one", () => {
    const anonymous = { ...note("passed"), checked_by: "  " };
    const b = freshnessBadge(fresh({ verification: anonymous }));
    expect(b.kind).toBe("fresh");
    expect(b.detail).not.toContain("by  ");
  });
});

describe("confidencePercent", () => {
  it("is null exactly when confidence was not measured", () => {
    expect(confidencePercent(fresh({ confidence: null }))).toBeNull();
    expect(confidencePercent(fresh({ confidence: 1 }))).toBe(100);
    expect(confidencePercent(fresh({ confidence: 0.769 }))).toBe(77);
    expect(confidencePercent(fresh({ confidence: 0 }))).toBe(0);
  });
});

describe("isNoteworthy", () => {
  it("stays quiet only for a commit we know carries nothing", () => {
    expect(isNoteworthy(freshnessBadge(fresh()))).toBe(false);
    for (const f of [
      fresh({ notes_readable: false }),
      fresh({ verification: note("passed") }),
      fresh({ verification: note("failed") }),
      fresh({ session: session() }),
    ]) {
      expect(isNoteworthy(freshnessBadge(f)), freshnessBadge(f).kind).toBe(true);
    }
  });
});
