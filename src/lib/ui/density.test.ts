import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { DENSITY_SURFACES, rowHeight, type DensitySurface } from "./density";
import { branchRowHeight } from "../sidebar/metrics";

const read = (path: string) =>
  readFileSync(new URL(`../components/${path}`, import.meta.url), "utf8");

describe("rowHeight", () => {
  it.each(DENSITY_SURFACES)("gives %s a tighter row when compact", (surface) => {
    expect(rowHeight(surface, "compact")).toBeLessThan(rowHeight(surface, "spacious"));
  });

  it.each(DENSITY_SURFACES)("keeps %s tall enough to be legible", (surface) => {
    // Below about 16px monospace text starts clipping descenders.
    expect(rowHeight(surface, "compact")).toBeGreaterThanOrEqual(16);
  });

  it("falls back to spacious rather than returning a NaN height", () => {
    // A NaN row height collapses a virtual list to an empty window, which
    // reads to the user as "this file has no content".
    const bogus = "gigantic" as unknown as "compact";
    for (const surface of DENSITY_SURFACES) {
      expect(Number.isFinite(rowHeight(surface, bogus))).toBe(true);
      expect(rowHeight(surface, bogus)).toBe(rowHeight(surface, "spacious"));
    }
  });

  it("keeps spacious identical to the constants each pane used to hard-code", () => {
    // Turning the setting to Spacious must reproduce today's layout exactly;
    // Compact is the only new geometry this introduces.
    const previous: Record<DensitySurface, number> = {
      diff: 20,
      code: 20,
      fileTree: 24,
      blame: 24,
      coverageFile: 26,
      coverageSource: 24,
    };
    for (const [surface, height] of Object.entries(previous)) {
      expect(rowHeight(surface as DensitySurface, "spacious")).toBe(height);
    }
  });
});

describe("the density setting reaches every fixed-row surface", () => {
  const PANES: Array<[string, string]> = [
    ["DiffViewer.svelte", "diff"],
    ["files/CodeViewer.svelte", "code"],
    ["files/FileTreePanel.svelte", "fileTree"],
    ["BlameViewer.svelte", "blame"],
    ["CoverageViewer.svelte", "coverageFile"],
  ];

  it.each(PANES)("%s sizes its rows from the shared owner", (file, surface) => {
    const source = read(file);
    expect(source).toContain("rowHeight(");
    expect(source).toContain(`"${surface}"`);
    expect(source).toContain("$densityStore");
  });

  it.each(PANES)("%s no longer hard-codes a row height", (file) => {
    const source = read(file);
    expect(source).not.toMatch(/const ROW_HEIGHT = \d+;/);
    expect(source).not.toMatch(/rowHeight=\{2[0-9]\}/);
  });

  it("agrees with the sidebar about which direction compact goes", () => {
    // Two owners disagreeing about what one step of density means is how the
    // commit list ended up tighter per step than the branch list.
    expect(branchRowHeight("compact")).toBeLessThan(branchRowHeight("spacious"));
  });
});
