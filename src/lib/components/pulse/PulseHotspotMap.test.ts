import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseHotspotMap.svelte", import.meta.url), "utf8");

/**
 * "High churn, low coverage" is only meaningful if "no coverage data" is kept
 * distinct from "0% covered". A check that could not run must never look like
 * a check that ran and failed the file.
 */
describe("PulseHotspotMap component", () => {
  it("delegates risk ranking to the tested metric", () => {
    expect(source).toContain("computeHotspotRisks");
  });

  it("separates a failed scan, a pending scan and an absent report", () => {
    expect(source).toContain("coverageFailed");
    expect(source).toContain("coveragePending");
    expect(source).toContain("{:else if !coverageReport}");
  });

  it("says plainly that a failed scan is not treated as zero coverage", () => {
    expect(source).toContain("missing coverage is not treated as 0%");
  });

  it("states a failed coverage scan in the unknown tone", () => {
    expect(source).toContain('class="gp-notice mt-1" data-tone="unknown"');
    expect(source).toContain("Couldn't scan coverage.");
  });

  it("carries both numbers when filters narrow the ranked list", () => {
    expect(source).toContain("Showing {filteredHotspots.length} of {allHotspots.length} ranked");
  });

  it("keeps the level filter a toggle: choosing the active level clears it", () => {
    expect(source).toContain('selectedLevel = selectedLevel === next ? "all" : next');
  });

  it("renders no percentage at all when a file has no coverage record", () => {
    expect(source).toContain("hotspot.coveragePercentage !== null");
    expect(source).toContain('hotspot.coverageStatus === "unscanned"');
  });
});
