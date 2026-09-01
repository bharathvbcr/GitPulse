import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./BranchHealthDot.svelte", import.meta.url), "utf8");

/**
 * The verdicts themselves are covered in branches/health.test.ts. This pins
 * the two decisions the component makes: that it delegates scoring, and that
 * it only draws for branches worth acting on.
 */
describe("BranchHealthDot", () => {
  it("delegates scoring rather than reimplementing thresholds", () => {
    expect(source).toContain("branchHealth");
    expect(source).not.toMatch(/\b30\b/);
    expect(source).not.toContain("86_400");
    expect(source).not.toContain("last_commit_timestamp");
  });

  it("draws only for branches that need attention", () => {
    expect(source).toContain("{#if needsAttention(health)}");
  });

  it("carries the explanation in both the tooltip and the accessible name", () => {
    // C1 requires the indicator to explain its verdict, and a title alone is
    // not reachable by a screen reader.
    expect(source).toContain("aria-label=");
    expect(source).toContain("title=");
    expect(source).toContain("health.detail");
    expect(source).toContain('role="img"');
  });

  it("takes `now` as a prop so it is deterministic under test", () => {
    expect(source).toMatch(/now\?: number/);
  });

  it("has a tone for every health level", () => {
    for (const level of ["healthy", "info", "warn", "attention"]) {
      expect(source).toContain(`${level}:`);
    }
  });
});
