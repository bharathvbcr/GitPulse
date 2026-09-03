import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseRhythm.svelte", import.meta.url), "utf8");

/**
 * Streak arithmetic is covered in pulse/metrics.test.ts. What matters here is
 * that the view stays a thin renderer, takes `now` so it is deterministic, and
 * says out loud that a bounded scan is why a gap looks the length it does.
 */
describe("PulseRhythm component", () => {
  it("delegates rhythm computation rather than recomputing it", () => {
    expect(source).toContain("computeRhythm");
    // no hand-rolled day arithmetic in the view layer
    expect(source).not.toContain("86400");
    expect(source).not.toContain("86_400");
  });

  it("takes `now` as a prop so rendering is deterministic under test", () => {
    expect(source).toMatch(/now\?: number/);
  });

  it("qualifies streak and gap when the scan was truncated", () => {
    expect(source).toMatch(/truncated\?: boolean/);
    expect(source).toContain("{#if truncated}");
    // The honest wording: a scan boundary must not read as a real quiet spell.
    expect(source).toContain("scanned window");
  });
});
