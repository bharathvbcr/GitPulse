import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseLineTrend.svelte", import.meta.url), "utf8");

/**
 * The LOC anchor comes from a scan that can be partial or fail outright. A
 * failed scan rendering as "0 LOC" is the exact honesty bug this view was
 * fixed for, so each state must stay separately rendered.
 */
describe("PulseLineTrend component", () => {
  it("delegates both series to tested metrics", () => {
    expect(source).toContain("computeLineChanges");
    expect(source).toContain("computeLocTrend");
  });

  it("distinguishes every LOC scan state", () => {
    expect(source).toMatch(/locStatus\?: "idle" \| "loading" \| "ok" \| "partial" \| "failed"/);
    expect(source).toContain('locStatus === "failed"');
    expect(source).toContain('locStatus === "loading"');
    expect(source).toContain('"partial"');
  });

  it("only reconstructs the LOC trend when a total is actually known", () => {
    expect(source).toMatch(/locStatus === "ok" \|\| locStatus === "partial"\s*\?\s*computeLocTrend/);
  });

  it("labels each weekly column for assistive tech", () => {
    expect(source).toContain('role="img"');
    expect(source).toContain("aria-label=");
  });
});
