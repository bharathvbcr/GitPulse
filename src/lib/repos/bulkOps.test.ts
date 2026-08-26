import { describe, expect, it } from "vitest";
import { summarizeBulkOutcome } from "./bulkOps";

describe("summarizeBulkOutcome", () => {
  it("reports success when nothing was requested", () => {
    expect(summarizeBulkOutcome([], "staged")).toEqual({ ok: true });
  });

  it("reports success when every file succeeded", () => {
    expect(summarizeBulkOutcome([{ ok: true }, { ok: true }], "staged")).toEqual({ ok: true });
  });

  // Regression: the pre-fix bulk loop discarded per-file outcomes and
  // returned { ok: true } even when every single file failed.
  it("reports failure with a count and first error when some files fail", () => {
    const outcome = summarizeBulkOutcome(
      [
        { ok: true },
        { ok: false, error: "index.lock exists" },
        { ok: false, error: "permission denied" },
      ],
      "staged",
    );
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("staged 1 of 3");
    expect(outcome.error).toContain("index.lock exists");
    expect(outcome.error).toContain("(+1 more staged failure)");
  });

  it("omits the more-suffix for a single failure", () => {
    const outcome = summarizeBulkOutcome(
      [{ ok: false, error: "boom" }],
      "unstaged",
    );
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toBe("unstaged 0 of 1: boom");
  });

  it("survives failures that carry no error text", () => {
    const outcome = summarizeBulkOutcome([{ ok: false }], "staged");
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("unknown error");
  });
});
