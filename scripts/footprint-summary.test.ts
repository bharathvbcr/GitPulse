import { describe, expect, it } from "vitest";
import { formatFootprintSummary, parseFootprint } from "./footprint-summary.mjs";

// The category block of `footprint -v -p 53227` on 2026-10-15, plus one region
// line that must not be read as the summary. Bytes below are the figures
// footprint printed, not a recount of the region list.
const SAMPLE = `
       104a44000 -        104a5c000 [7443de65cd]       6       4       0       0   WebKit Malloc

  Dirty      Clean  Reclaimable    Regions    Category
    ---        ---          ---        ---    ---
 632 MB        0 B       140 MB        201    WebKit Malloc
 257 MB        0 B          0 B       4297    Untagged
 148 MB        0 B       174 MB        398    Owned physical footprint (unmapped) (graphics)
  12 MB        0 B       304 KB       1278    Malloc Small
3557 KB        0 B          0 B          1    page table
    ---        ---          ---        ---    ---
1094 MB      11 MB       316 MB      13546    TOTAL

Auxiliary data:
    phys_footprint: 1094 MB
    phys_footprint_peak: 1905 MB
`;

const MB = 1024 * 1024;

describe("footprint summary", () => {
  it("reads WebKit Malloc from the category summary, not from a region line", () => {
    const summary = parseFootprint(SAMPLE);
    expect(summary.webkitMalloc).toEqual({
      dirtyBytes: 632 * MB,
      cleanBytes: 0,
      reclaimableBytes: 140 * MB,
      regions: 201,
    });
    expect(summary.physFootprintBytes).toBe(1094 * MB);
    expect(summary.physFootprintPeakBytes).toBe(1905 * MB);
    expect(summary.categories["Untagged"]?.dirtyBytes).toBe(257 * MB);
    expect(summary.categories["Owned physical footprint (unmapped) (graphics)"]?.reclaimableBytes).toBe(174 * MB);
    expect(summary.categories["Malloc Small"]?.reclaimableBytes).toBe(304 * 1024);
    expect(summary.categories["page table"]?.dirtyBytes).toBe(3557 * 1024);
    expect(summary.categories["TOTAL"]?.dirtyBytes).toBe(1094 * MB);
  });

  it("reports a missing category as absent, not zero", () => {
    const summary = parseFootprint("    phys_footprint: 4 KB\n  1 GB        0 B          0 B          1    Untagged\n");
    expect(summary.webkitMalloc).toBeNull();
    expect(summary.physFootprintBytes).toBe(4 * 1024);
    expect(summary.physFootprintPeakBytes).toBeNull();
    expect(summary.categories["Untagged"]?.dirtyBytes).toBe(1024 * MB);
    expect(summary.categories["WebKit Malloc"]).toBeUndefined();
    expect(formatFootprintSummary(summary)).toBe(
      ["phys_footprint: 0.00 MB", "phys_footprint_peak: absent", "WebKit Malloc: absent"].join("\n"),
    );
  });

  it("accepts fractional units and an empty report", () => {
    const summary = parseFootprint("  1.5 KB        2 GB          0 B          3    WebKit Malloc\n");
    expect(summary.webkitMalloc).toEqual({
      dirtyBytes: Math.round(1.5 * 1024),
      cleanBytes: 2 * 1024 * MB,
      reclaimableBytes: 0,
      regions: 3,
    });
    expect(formatFootprintSummary(parseFootprint(""))).toContain("WebKit Malloc: absent");
    expect(formatFootprintSummary(parseFootprint(SAMPLE))).toContain("WebKit Malloc dirty: 632.00 MB");
    expect(formatFootprintSummary(parseFootprint(SAMPLE))).toContain("WebKit Malloc regions: 201");
  });
});
