import { describe, expect, it } from "vitest";
import {
  blockedInitializeReason,
  canDeinit,
  canInitialize,
  canSync,
  describeSubmodules,
  initializableSubmodules,
  isDestructiveSubmoduleChange,
  needsAttention,
  parseSubmoduleList,
  sortSubmodules,
  submoduleChangeConsequence,
  submoduleStateExplanation,
  submoduleStateLabel,
  type SubmoduleChange,
  type SubmoduleInfo,
  type SubmoduleState,
} from "./submodules";

const ALL_STATES: SubmoduleState[] = [
  "Uninitialized",
  "UpToDate",
  "CommitDiffers",
  "Conflicted",
];

function sub(extra: Partial<SubmoduleInfo> = {}): SubmoduleInfo {
  return {
    name: "the-lib",
    path: "vendor/lib",
    url: "https://example.test/lib.git",
    oid: "abc123",
    described: "heads/main",
    state: "UpToDate",
    orphaned: false,
    ...extra,
  };
}

describe("state wording", () => {
  it("labels and explains every state", () => {
    for (const state of ALL_STATES) {
      expect(submoduleStateLabel(state)).toBeTruthy();
      expect(submoduleStateExplanation(state)).toBeTruthy();
      // The enum identifier must never reach the screen.
      expect(submoduleStateLabel(state)).not.toBe(state);
    }
  });

  it("leads with the symptom the user is actually looking at", () => {
    // "Uninitialized" means nothing; "its folder is empty" is the thing on
    // screen that sent them here.
    expect(submoduleStateExplanation("Uninitialized")).toContain("folder is empty");
  });

  it("warns that a moved submodule will change what this repo records", () => {
    expect(submoduleStateExplanation("CommitDiffers")).toContain("move the recorded pointer");
  });
});

describe("needsAttention", () => {
  it("flags everything except an up-to-date submodule", () => {
    expect(needsAttention(sub({ state: "UpToDate" }))).toBe(false);
    for (const state of ALL_STATES.filter((s) => s !== "UpToDate")) {
      expect(needsAttention(sub({ state })), state).toBe(true);
    }
  });
});

describe("canInitialize", () => {
  it("offers initialization for an ordinary uninitialized submodule", () => {
    expect(canInitialize(sub({ state: "Uninitialized" }))).toBe(true);
    expect(blockedInitializeReason(sub({ state: "Uninitialized" }))).toBeNull();
  });

  it("refuses an orphaned entry and explains why the button would not work", () => {
    // Present in the index, absent from .gitmodules: there is no URL to fetch
    // from, so "Initialize" could only ever fail.
    const orphan = sub({ state: "Uninitialized", orphaned: true, url: null });
    expect(canInitialize(orphan)).toBe(false);
    expect(blockedInitializeReason(orphan)).toContain(".gitmodules");
  });

  it("does not offer initialization for an already-checked-out submodule", () => {
    expect(canInitialize(sub({ state: "UpToDate" }))).toBe(false);
    expect(canInitialize(sub({ state: "CommitDiffers" }))).toBe(false);
    expect(blockedInitializeReason(sub({ state: "UpToDate" }))).toBeNull();
  });
});

describe("canDeinit and canSync", () => {
  it("offers deinit only where there is a working copy to remove", () => {
    expect(canDeinit(sub({ state: "UpToDate" }))).toBe(true);
    expect(canDeinit(sub({ state: "CommitDiffers" }))).toBe(true);
    expect(canDeinit(sub({ state: "Conflicted" }))).toBe(true);
    expect(canDeinit(sub({ state: "Uninitialized" }))).toBe(false);
  });

  it("offers sync only when .gitmodules still names a URL", () => {
    expect(canSync(sub())).toBe(true);
    expect(canSync(sub({ url: null }))).toBe(false);
    expect(canSync(sub({ orphaned: true }))).toBe(false);
  });
});

describe("initializableSubmodules", () => {
  it("excludes orphans so a bulk action cannot report a failure it cannot fix", () => {
    const list = [
      sub({ path: "a", state: "Uninitialized" }),
      sub({ path: "b", state: "Uninitialized", orphaned: true }),
      sub({ path: "c", state: "UpToDate" }),
    ];
    expect(initializableSubmodules(list).map((s) => s.path)).toEqual(["a"]);
  });

  it("returns nothing when there is nothing to initialize", () => {
    expect(initializableSubmodules([sub()])).toEqual([]);
    expect(initializableSubmodules([])).toEqual([]);
  });
});

describe("change consequences", () => {
  const changes: SubmoduleChange[] = [
    { kind: "update", path: null, recursive: true },
    { kind: "update", path: "vendor/lib", recursive: false },
    { kind: "sync", path: null, recursive: false },
    { kind: "deinit", path: "vendor/lib", force: true },
  ];

  it("describes every change", () => {
    for (const change of changes) {
      expect(submoduleChangeConsequence(change), change.kind).toBeTruthy();
    }
  });

  it("treats deinit as destructive and says what is lost", () => {
    expect(isDestructiveSubmoduleChange({ kind: "deinit", path: "x", force: false })).toBe(true);
    expect(isDestructiveSubmoduleChange({ kind: "update", path: null, recursive: false })).toBe(false);
    expect(isDestructiveSubmoduleChange({ kind: "sync", path: null, recursive: false })).toBe(false);
    const text = submoduleChangeConsequence({ kind: "deinit", path: "vendor/lib", force: true });
    expect(text).toContain("Uncommitted changes inside it are lost");
    expect(text).toContain("vendor/lib");
  });

  it("distinguishes updating one submodule from updating all of them", () => {
    const one = submoduleChangeConsequence({ kind: "update", path: "vendor/lib", recursive: false });
    const all = submoduleChangeConsequence({ kind: "update", path: null, recursive: false });
    expect(one).toContain("this submodule");
    expect(all).toContain("every submodule");
  });
});

describe("describeSubmodules", () => {
  it("says plainly when there are none", () => {
    expect(describeSubmodules([])).toBe("This repository has no submodules.");
  });

  it("reports a healthy set", () => {
    expect(describeSubmodules([sub()])).toBe("1 submodule, up to date.");
    expect(describeSubmodules([sub(), sub({ path: "b" })])).toBe("2 submodules, all up to date.");
  });

  it("leads with the uninitialized count, the state users actually hit", () => {
    const list = [sub({ path: "a", state: "Uninitialized" }), sub({ path: "b" })];
    expect(describeSubmodules(list)).toBe("1 of 2 submodules not initialized.");
  });

  it("falls back to a general count when the problems are mixed", () => {
    const list = [
      sub({ path: "a", state: "Uninitialized" }),
      sub({ path: "b", state: "Conflicted" }),
      sub({ path: "c" }),
    ];
    expect(describeSubmodules(list)).toBe("2 of 3 submodules need attention.");
  });

  it("uses the singular for a lone broken submodule", () => {
    expect(describeSubmodules([sub({ state: "Uninitialized" })])).toBe(
      "1 of 1 submodule not initialized.",
    );
  });
});

describe("sortSubmodules", () => {
  it("puts the broken ones first, worst state leading", () => {
    const list = [
      sub({ path: "d", state: "UpToDate" }),
      sub({ path: "c", state: "CommitDiffers" }),
      sub({ path: "b", state: "Uninitialized" }),
      sub({ path: "a", state: "Conflicted" }),
    ];
    expect(sortSubmodules(list).map((s) => s.path)).toEqual(["a", "b", "c", "d"]);
  });

  it("orders by path within one state, for a stable list", () => {
    const list = [
      sub({ path: "z", state: "Uninitialized" }),
      sub({ path: "a", state: "Uninitialized" }),
    ];
    expect(sortSubmodules(list).map((s) => s.path)).toEqual(["a", "z"]);
  });

  it("does not mutate its input", () => {
    const list = [sub({ path: "b", state: "UpToDate" }), sub({ path: "a", state: "Conflicted" })];
    const before = list.map((s) => s.path);
    sortSubmodules(list);
    expect(list.map((s) => s.path)).toEqual(before);
  });
});

describe("parseSubmoduleList", () => {
  it("unwraps a complete listing", () => {
    const parsed = parseSubmoduleList({
      submodules: [sub()],
      truncated: false,
    });
    expect(parsed.failed).toBe(false);
    expect(parsed.truncated).toBe(false);
    expect(parsed.submodules).toHaveLength(1);
    expect(parsed.submodules[0].path).toBe("vendor/lib");
  });

  it("treats a bare array as a failed read, not an empty submodule list", () => {
    const parsed = parseSubmoduleList([]);
    expect(parsed.failed).toBe(true);
    expect(parsed.submodules).toEqual([]);
  });

  it("fails closed when truncated is missing or a state is unknown", () => {
    expect(parseSubmoduleList({ submodules: [sub()] }).failed).toBe(true);
    expect(
      parseSubmoduleList({
        submodules: [
          {
            name: "x",
            path: "x",
            url: null,
            oid: null,
            described: null,
            state: "Nope",
            orphaned: false,
          },
        ],
        truncated: false,
      }).failed,
    ).toBe(true);
  });
});
