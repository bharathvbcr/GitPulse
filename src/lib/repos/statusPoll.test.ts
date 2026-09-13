import { describe, expect, it } from "vitest";
import {
  shallowRecordListEqual,
  shouldRunStatusPoll,
  statusesEqual,
  type StatusLike,
} from "./statusPoll";

function status(partial: Partial<StatusLike> & { path: string }): StatusLike {
  return {
    old_path: undefined,
    status_code: "M",
    is_staged: false,
    is_conflicted: false,
    additions: 0,
    deletions: 0,
    ...partial,
  };
}

describe("statusesEqual", () => {
  it("treats two empty lists as equal", () => {
    expect(statusesEqual([], [])).toBe(true);
  });

  it("detects length differences first", () => {
    const single = [status({ path: "a.ts" })];
    expect(statusesEqual(single, [])).toBe(false);
    expect(statusesEqual([], single)).toBe(false);
    expect(statusesEqual(single, [status({ path: "a.ts" }), status({ path: "b.ts" })])).toBe(false);
  });

  it("compares strictly index-wise: a reordered multiset counts as changed", () => {
    // Deliberate, documented choice: this function only gates PUBLISHES, so a
    // reorder costing one extra publish is the conservative failure mode.
    // Multiset matching could miss a duplicate-path entry flipping sides.
    const ordered = [status({ path: "a.ts" }), status({ path: "b.ts" })];
    const swapped = [status({ path: "b.ts" }), status({ path: "a.ts" })];
    expect(statusesEqual(ordered, swapped)).toBe(false);
  });

  it("ignores object identity: fresh copies with equal fields are equal", () => {
    const a = [status({ path: "a.ts", additions: 3 }), status({ path: "b.ts", deletions: 1 })];
    const b = a.map((item) => ({ ...item }));
    expect(a).not.toBe(b);
    expect(a[0]).not.toBe(b[0]);
    expect(statusesEqual(a, b)).toBe(true);
  });

  /**
   * Derived from the type, not hand-listed. The hand-listed version of this
   * test fell behind the wire the moment Rust grew `warnings`: the gate kept
   * comparing the other eleven fields, so a row whose churn stopped being
   * partial compared equal, the publish was skipped, and the explorer went on
   * rendering "counts may understate" for numbers that were now known-good.
   * Walking a fully populated row means the next field added is covered here
   * without anyone remembering to edit this list.
   */
  it("detects a change in every field the wire carries", () => {
    const populated: Required<StatusLike> = {
      path: "a.ts",
      old_path: "z.ts",
      status_code: "M",
      is_staged: true,
      is_conflicted: true,
      additions: 3,
      deletions: 4,
      staged_additions: 1,
      staged_deletions: 2,
      unstaged_additions: 5,
      unstaged_deletions: 6,
      warnings: ["numstat record had unparseable counts"],
    };
    const keys = Object.keys(populated) as Array<keyof StatusLike>;
    expect(keys.length, "populate every field of StatusLike").toBe(12);

    for (const key of keys) {
      const value = populated[key];
      const changed =
        typeof value === "string"
          ? `${value}-other`
          : typeof value === "number"
            ? value + 1
            : typeof value === "boolean"
              ? !value
              : Array.isArray(value)
                ? []
                : value;
      expect(statusesEqual([populated], [{ ...populated, [key]: changed }]), key).toBe(false);
    }
  });

  /**
   * The exact shape the gate has to catch. Rust falls back to `(0, 0)` churn
   * for a numstat record it cannot parse and attaches a warning, so when that
   * record later parses on a genuinely zero-churn row — a mode-only change —
   * every other field is identical and only `warnings` empties.
   */
  it("republishes when churn stops being partial but the counts do not move", () => {
    const partial = [status({ path: "a.ts", warnings: ["numstat record had unparseable counts"] })];
    const trustworthy = [status({ path: "a.ts" })];
    expect(statusesEqual(partial, trustworthy)).toBe(false);
  });

  /** Rust omits the key entirely while empty, so absent and `[]` mean the same. */
  it("treats an absent warnings list and an empty one as equal", () => {
    const absent = [status({ path: "a.ts" })];
    const empty = [status({ path: "a.ts", warnings: [] })];
    expect(statusesEqual(absent, empty)).toBe(true);
  });

  it("keeps a missing old_path distinct from an empty rename source", () => {
    const absent = [status({ path: "a.ts", old_path: undefined })];
    const empty = [status({ path: "a.ts", old_path: "" })];
    expect(statusesEqual(absent, empty)).toBe(false);
  });
});

describe("shouldRunStatusPoll", () => {
  it("runs only when a visible, idle session exists and nothing is in flight", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: false,
        inflight: false,
      })
    ).toBe(true);
  });

  it("skips when the window is hidden", () => {
    expect(
      shouldRunStatusPoll({
        hidden: true,
        hasSession: true,
        isLoading: false,
        inflight: false,
      })
    ).toBe(false);
  });

  it("skips with no open repository", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: false,
        isLoading: false,
        inflight: false,
      })
    ).toBe(false);
  });

  it("never overlaps a hydrate/refresh or the previous poll", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: true,
        inflight: false,
      })
    ).toBe(false);
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: false,
        inflight: true,
      })
    ).toBe(false);
  });
});

describe("shallowRecordListEqual", () => {
  it("treats empty lists as equal and length mismatches as different", () => {
    expect(shallowRecordListEqual([], [])).toBe(true);
    expect(shallowRecordListEqual([{ a: 1 }], [])).toBe(false);
  });

  it("compares only the declared fields with value equality", () => {
    const fields = ["name", "tip"] as const;
    const a = [{ name: "main", tip: "abc", extra: "ignored" }];
    const b = [{ name: "main", tip: "abc", extra: "DIFFERENT" }];
    expect(shallowRecordListEqual(a, b, fields)).toBe(true);
  });

  it("detects any declared field changing on any element", () => {
    const fields = ["name", "tip"] as const;
    expect(
      shallowRecordListEqual([{ name: "main", tip: "abc" }], [{ name: "dev", tip: "abc" }], fields),
    ).toBe(false);
    expect(
      shallowRecordListEqual([{ name: "main", tip: "abc" }], [{ name: "main", tip: "def" }], fields),
    ).toBe(false);
  });

  it("ignores identity: fresh clones with equal fields are equal", () => {
    const fields = ["n"] as const;
    const a = [{ n: 1 }, { n: 2 }];
    const b = a.map((item) => ({ ...item }));
    expect(a).not.toBe(b);
    expect(shallowRecordListEqual(a, b, fields)).toBe(true);
  });
});


it("publishes side churn changes even when total churn and porcelain are unchanged", () => {
  const before = status({ path: "mixed", status_code: "MM", is_staged: true, additions: 3, staged_additions: 1, unstaged_additions: 2 });
  const after = { ...before, staged_additions: 2, unstaged_additions: 1 };
  expect(statusesEqual([before], [after])).toBe(false);
  expect(statusesEqual([after], [{ ...after }])).toBe(true);
});
