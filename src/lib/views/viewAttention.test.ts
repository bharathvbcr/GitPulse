import { describe, expect, it } from "vitest";
import { sectionAttention, viewAttention, type AttentionContext } from "./viewAttention";
import { sectionsFor } from "./viewRegistry";

const calm: AttentionContext = { conflictedCount: 0, verdictStatus: null };

describe("sectionAttention", () => {
  it("puts the conflict count on Policy, which holds the conflict editor, and only there", () => {
    const ctx = { ...calm, conflictedCount: 3 };
    expect(sectionAttention("work", "policy", ctx)).toEqual({
      tone: "rose",
      count: 3,
      reason: "3 files still carry conflict markers",
    });
    expect(sectionAttention("work", "overview", ctx)).toBeNull();
    expect(sectionAttention("history", "policy", ctx)).toBeNull();
    // Resolve is retired; its id is no longer a section anything is drawn for.
    expect(sectionAttention("work", "resolve", ctx)).toBeNull();
  });

  it("agrees in number for a single file", () => {
    expect(sectionAttention("work", "policy", { ...calm, conflictedCount: 1 })?.reason)
      .toBe("1 file still carries conflict markers");
  });

  it("lets conflict markers outrank a blocked verdict", () => {
    expect(sectionAttention("work", "policy", { conflictedCount: 2, verdictStatus: "blocked" })?.count)
      .toBe(2);
  });

  it("marks Policy only for a blocked verdict", () => {
    expect(sectionAttention("work", "policy", { ...calm, verdictStatus: "blocked" }))
      .toEqual({ tone: "rose", count: null, reason: "MANVI blocked the last action" });
    for (const status of ["allowed", "warned", "unchecked", "degraded"] as const) {
      expect(sectionAttention("work", "policy", { ...calm, verdictStatus: status }), status).toBeNull();
    }
  });

  it("is silent when nothing needs the reader", () => {
    expect(sectionAttention("work", "policy", calm)).toBeNull();
  });
});

describe("viewAttention over the registry's live sections", () => {
  // The switcher passes the sections the registry offers, so attention keyed
  // to a section that was folded away is attention nobody sees. Resolve folded
  // into Policy: conflict markers must still raise Work's pill.
  const work = sectionsFor("work").map((s) => s.id);

  it("raises Work's pill for conflict markers", () => {
    expect(viewAttention("work", work, { ...calm, conflictedCount: 2 })).toEqual({
      tone: "rose",
      count: 2,
      reason: "2 files still carry conflict markers",
    });
  });

  it("names a section the registry still offers", () => {
    for (const ctx of [
      { ...calm, conflictedCount: 1 },
      { ...calm, verdictStatus: "blocked" as const },
    ]) {
      const owner = work.find((section) => sectionAttention("work", section, ctx));
      expect(owner, JSON.stringify(ctx)).toBe("policy");
    }
  });
});

describe("viewAttention", () => {
  it("lifts the first section's attention onto the view's pill", () => {
    const ctx = { conflictedCount: 2, verdictStatus: "blocked" as const };
    expect(viewAttention("work", ["overview", "policy"], ctx)?.count).toBe(2);
    expect(viewAttention("work", ["overview", "policy"], { ...ctx, conflictedCount: 0 })?.reason)
      .toBe("MANVI blocked the last action");
    expect(viewAttention("code", ["explorer", "map"], ctx)).toBeNull();
  });
});
