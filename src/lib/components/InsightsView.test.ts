import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { sectionsFor } from "../views/viewRegistry";
import { RETIRED_VIEWS } from "../repos/persist";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "InsightsView.svelte"), "utf8");

describe("InsightsView", () => {
  it("hosts the four scans that were four header entries", () => {
    expect(sectionsFor("insights").map((s) => s.id)).toEqual([
      "pulse",
      "coverage",
      "health",
      "storage",
    ]);
  });

  it("keeps every pane lazy", () => {
    // Opening Insights for the activity heatmap must not pay for the coverage
    // parser, the dependency auditor or the disk walker. Four loaders in, four
    // LazyView render sites, no static import of a panel.
    for (const loader of ["loadPulse", "loadCoverage", "loadHealth", "loadStorage"]) {
      expect(source).toContain(`${loader}: ViewLoader`);
      expect(source).toContain(`load={${loader}}`);
    }
    expect(source).not.toMatch(/import\s+\w*(Panel|Viewer)\s+from/);
  });

  it("opens on Pulse when nothing is remembered", () => {
    // The {:else} arm is the default, and it must agree with the registry's
    // first section or the segmented control would highlight the wrong pane.
    expect(source).toContain("{:else}");
    expect(source).toContain('load={loadPulse}');
    expect(sectionsFor("insights")[0].id).toBe("pulse");
  });

  it("gives each merged view a section that a restored session lands on", () => {
    for (const id of ["pulse", "coverage", "health", "storage"]) {
      expect(RETIRED_VIEWS[id], `${id} was not recorded as retired`).toEqual({
        tab: "insights",
        section: id,
      });
    }
  });
});
