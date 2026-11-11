import { describe, expect, it } from "vitest";
import { sectionAttention, viewAttention, type AttentionContext } from "./viewAttention";

const calm: AttentionContext = { conflictedCount: 0, verdictStatus: null };

describe("sectionAttention", () => {
  it("puts the conflict count on Resolve, and only there", () => {
    const ctx = { ...calm, conflictedCount: 3 };
    expect(sectionAttention("work", "resolve", ctx)).toEqual({
      tone: "rose",
      count: 3,
      reason: "3 files still carry conflict markers",
    });
    expect(sectionAttention("work", "overview", ctx)).toBeNull();
    expect(sectionAttention("history", "resolve", ctx)).toBeNull();
  });

  it("agrees in number for a single file", () => {
    expect(sectionAttention("work", "resolve", { ...calm, conflictedCount: 1 })?.reason)
      .toBe("1 file still carries conflict markers");
  });

  it("marks Policy only for a blocked verdict", () => {
    expect(sectionAttention("work", "policy", { ...calm, verdictStatus: "blocked" }))
      .toEqual({ tone: "rose", count: null, reason: "Manvi blocked the last action" });
    for (const status of ["allowed", "warned", "unchecked", "degraded"] as const) {
      expect(sectionAttention("work", "policy", { ...calm, verdictStatus: status }), status).toBeNull();
    }
  });

  it("is silent when nothing needs the reader", () => {
    expect(sectionAttention("work", "resolve", calm)).toBeNull();
    expect(sectionAttention("work", "policy", calm)).toBeNull();
  });
});

describe("viewAttention", () => {
  it("lifts the first section's attention onto the view's pill", () => {
    const ctx = { conflictedCount: 2, verdictStatus: "blocked" as const };
    expect(viewAttention("work", ["overview", "resolve", "policy"], ctx)?.count).toBe(2);
    expect(viewAttention("work", ["overview", "policy"], ctx)?.reason).toBe("Manvi blocked the last action");
    expect(viewAttention("code", ["explorer", "blame"], ctx)).toBeNull();
  });
});
