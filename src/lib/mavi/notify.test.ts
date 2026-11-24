import { describe, expect, it } from "vitest";
import { maviNotices, REPEAT_MS } from "./notify";
import type { ThreadStatus } from "./threads";

describe("maviNotices", () => {
  it("records the first status silently, so a reload never re-announces", () => {
    const seen = new Map<string, ThreadStatus>();
    expect(maviNotices(seen, [["a", "needs_you"], ["b", "failed"]])).toEqual([]);
    expect(maviNotices(seen, [["a", "needs_you"], ["b", "failed"]])).toEqual([]);
  });

  it("announces a request and a failure, never a finished turn", () => {
    const seen = new Map<string, ThreadStatus>([["a", "working"]]);
    const announced = new Map<string, number>();
    expect(maviNotices(seen, [["a", "needs_you"]], announced, 0)).toEqual([{ key: "a", kind: "needs_you" }]);
    expect(maviNotices(seen, [["a", "working"]], announced, 1)).toEqual([]);
    expect(maviNotices(seen, [["a", "review"]], announced, 2)).toEqual([]);
    seen.set("a", "working");
    expect(maviNotices(seen, [["a", "failed"]], announced, 3)).toEqual([{ key: "a", kind: "failed" }]);
  });

  it("does not repeat the same request within the window", () => {
    const seen = new Map<string, ThreadStatus>([["a", "working"]]);
    const announced = new Map<string, number>();
    expect(maviNotices(seen, [["a", "needs_you"]], announced, 0)).toHaveLength(1);
    maviNotices(seen, [["a", "working"]], announced, 10);
    expect(maviNotices(seen, [["a", "needs_you"]], announced, 20)).toHaveLength(0);
    maviNotices(seen, [["a", "working"]], announced, 30);
    expect(maviNotices(seen, [["a", "needs_you"]], announced, REPEAT_MS + 1)).toHaveLength(1);
  });

  it("forgets removed threads", () => {
    const seen = new Map<string, ThreadStatus>([["a", "idle"], ["gone", "working"]]);
    maviNotices(seen, [["a", "done"]]);
    expect(seen.has("gone")).toBe(false);
  });
});
