import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const blast = readFileSync(join(here, "BlastRadiusPanel.svelte"), "utf8");
const rung = readFileSync(join(here, "RungFilterControl.svelte"), "utf8");

describe("BlastRadiusPanel", () => {
  it("renders node_count and nodes_omitted, not only the sample", () => {
    expect(blast).toContain("node_count");
    expect(blast).toContain("nodes_omitted");
    expect(blast).toContain("unmatched_targets");
    expect(blast).toContain("walk_incomplete");
    expect(blast).toContain("line-clamp-3");
    expect(blast).toContain("tooltipWalkIncomplete");
    expect(blast).toContain("boundedJoin(layer.nodes");
    expect(blast).toContain("cancelled_seeds");
    expect(blast).toContain("isCancelledReason");
    expect(blast).toContain("partial answer");
    expect(blast).not.toContain("title={blast.walk_incomplete}");
    expect(blast).not.toContain("title={layer.nodes.join");
    expect(blast).not.toContain("minRung");
    expect(blast).not.toContain("RungFilter");
  });
});

describe("RungFilterControl", () => {
  it("hides when layered impact is active", () => {
    expect(rung).toContain("shouldShowRungControl");
    expect(rung).toContain("layeredImpactActive");
    expect(rung).toContain("rungHistogramLine");
  });
});
