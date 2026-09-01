import { describe, expect, it } from "vitest";
import {
  STASH_ACTIONS,
  isDestructiveStashAction,
  isStaleStackError,
  stackMatches,
  stashActionConsequence,
  stashActionLabel,
  stashActionPayload,
  stashEmptyMessage,
  stashSubtitle,
  stashTitle,
  type StashEntry,
} from "./stash";

function entry(extra: Partial<StashEntry> = {}): StashEntry {
  return {
    index: 0,
    selector: "stash@{0}",
    oid: "aabbccddeeff0011",
    subject: "On main: half-done parser",
    message: "half-done parser",
    branch: "main",
    timestamp: 1_700_000_000,
    ...extra,
  };
}

describe("labels", () => {
  it("covers every action with wording that stands alone", () => {
    for (const action of STASH_ACTIONS) {
      expect(stashActionLabel(action)).toBeTruthy();
      expect(stashActionConsequence(action)).toBeTruthy();
    }
  });

  it("does not use the word 'pop', which means nothing outside a terminal", () => {
    expect(stashActionLabel("pop")).toBe("Apply & remove");
    expect(stashActionLabel("apply")).toBe("Apply");
  });

  it("leads with whether the entry survives, the thing users get wrong", () => {
    expect(stashActionConsequence("apply")).toContain("keeps the stash entry");
    expect(stashActionConsequence("pop")).toContain("removes the stash entry");
    expect(stashActionConsequence("drop")).toContain("not recoverable");
  });
});

describe("destructiveness", () => {
  it("treats pop as destructive, not just drop", () => {
    // Pop removes the entry; if the restore then conflicts, the entry is gone
    // AND the changes are unresolved in the tree — the worst of both.
    expect(isDestructiveStashAction("pop")).toBe(true);
    expect(isDestructiveStashAction("drop")).toBe(true);
    expect(isDestructiveStashAction("apply")).toBe(false);
  });
});

describe("titles", () => {
  it("shows the user's message when there is one", () => {
    expect(stashTitle(entry())).toBe("half-done parser");
  });

  it("falls back through subject to selector rather than rendering blank", () => {
    expect(stashTitle(entry({ message: "  ", subject: "On main: something" }))).toBe(
      "On main: something",
    );
    expect(stashTitle(entry({ message: "", subject: "" }))).toBe("stash@{0}");
  });

  it("shows the branch and the short object id that actions are addressed by", () => {
    expect(stashSubtitle(entry())).toBe("on main · aabbccd");
  });

  it("omits the branch when git recorded none", () => {
    expect(stashSubtitle(entry({ branch: null }))).toBe("aabbccd");
  });
});

describe("stackMatches", () => {
  it("accepts an identical stack", () => {
    const a = [entry(), entry({ index: 1, oid: "1111", selector: "stash@{1}" })];
    const b = [entry(), entry({ index: 1, oid: "1111", selector: "stash@{1}" })];
    expect(stackMatches(a, b)).toBe(true);
  });

  it("rejects a stack whose entries shifted under the same indices", () => {
    // The exact hazard: indices look identical, but entry 0 is now somebody
    // else's stash. Every action computed from the old list is wrong.
    const before = [entry({ index: 0, oid: "aaaa" })];
    const after = [entry({ index: 0, oid: "bbbb" })];
    expect(stackMatches(before, after)).toBe(false);
  });

  it("rejects a stack that grew or shrank", () => {
    expect(stackMatches([entry()], [])).toBe(false);
    expect(stackMatches([], [entry()])).toBe(false);
  });

  it("treats two empty stacks as matching", () => {
    expect(stackMatches([], [])).toBe(true);
  });
});

describe("stashActionPayload", () => {
  it("returns the index and object id together, never one without the other", () => {
    expect(stashActionPayload(entry({ index: 3, oid: "abc123" }))).toEqual({
      index: 3,
      expectedOid: "abc123",
    });
  });

  it("refuses an entry with no usable object id", () => {
    // Without an oid the backend cannot verify the index, so the action would
    // fall back to trusting a number that may have shifted.
    expect(stashActionPayload(entry({ oid: "" }))).toBeNull();
    expect(stashActionPayload(entry({ oid: "not-hex!" }))).toBeNull();
  });

  it("refuses a nonsensical index", () => {
    expect(stashActionPayload(entry({ index: -1 }))).toBeNull();
    expect(stashActionPayload(entry({ index: 1.5 }))).toBeNull();
    expect(stashActionPayload(entry({ index: Number.NaN }))).toBeNull();
  });

  it("refuses a missing entry rather than throwing", () => {
    expect(stashActionPayload(null)).toBeNull();
    expect(stashActionPayload(undefined)).toBeNull();
  });
});

describe("isStaleStackError", () => {
  it("recognizes the backend's refusal so the UI can refresh instead of erroring", () => {
    expect(
      isStaleStackError(
        'Stash entry 2 changed since it was listed — it now holds "On main: other". Refresh the stash list and try again.',
      ),
    ).toBe(true);
    expect(
      isStaleStackError(
        "Stash entry 5 no longer exists — the stash stack now holds 2 entries. Refresh the stash list and try again.",
      ),
    ).toBe(true);
  });

  it("does not swallow unrelated failures", () => {
    expect(isStaleStackError("error: could not restore untracked files from stash")).toBe(false);
  });
});

describe("stashEmptyMessage", () => {
  it("separates an empty stack from an unloaded one", () => {
    expect(stashEmptyMessage(true)).toBe("No stashed changes.");
    expect(stashEmptyMessage(false)).toContain("not loaded");
  });
});
