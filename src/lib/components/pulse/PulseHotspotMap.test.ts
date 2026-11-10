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

describe("PulseHotspotMap rescan action", () => {
  it("offers Rescan on a failed coverage scan, only when the parent can rescan", async () => {
    const { render } = await import("svelte/server");
    const { default: PulseHotspotMap } = await import("./PulseHotspotMap.svelte");
    const props = { topFiles: [], coverageReport: null, coverageFailed: true };
    expect(render(PulseHotspotMap, { props }).body).not.toContain("Rescan");
    expect(render(PulseHotspotMap, { props: { ...props, onRescanCoverage: () => {} } }).body).toContain("Rescan");
    // A scan that has not failed has nothing to rescan.
    expect(
      render(PulseHotspotMap, { props: { ...props, coverageFailed: false, onRescanCoverage: () => {} } }).body,
    ).not.toContain("Rescan");
  });

  it("calls the parent's rescan, which forces the one shared coverage metric", () => {
    expect(source).toContain("onclick={() => onRescanCoverage?.()}");
    const view = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");
    expect(view).toContain("onRescanCoverage={rescanCoverage}");
    expect(view).toContain("coverageMetric.refresh(path, { force: true })");
  });
});
