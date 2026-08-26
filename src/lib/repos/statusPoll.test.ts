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

  it("detects every compared field changing", () => {
    const base = [status({ path: "a.ts", old_path: "z.ts" })];
    const variants: Array<Partial<StatusLike>> = [
      { path: "b.ts" },
      { old_path: "y.ts" },
      { status_code: "D" },
      { is_staged: true },
      { is_conflicted: true },
      { additions: 9 },
      { deletions: 9 },
    ];
    for (const variant of variants) {
      const changed = [status({ path: "a.ts", old_path: "z.ts", ...variant })];
      expect(statusesEqual(base, changed), JSON.stringify(variant)).toBe(false);
    }
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
