import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { render } from "svelte/server";
import BlastRadiusPanel from "./BlastRadiusPanel.svelte";
import type { ComposedBlastRadius } from "../codeintel/blastCompose";
import { QUERY_CANCELLED_REASON } from "../codeintel/blastCompose";

const here = dirname(fileURLToPath(import.meta.url));
const blastSource = readFileSync(join(here, "BlastRadiusPanel.svelte"), "utf8");
const rung = readFileSync(join(here, "RungFilterControl.svelte"), "utf8");

/** Verbatim kernel prose — the thing this panel exists to stop dumping on screen. */
const KERNEL_ESSAY =
  "the walk did not complete: stopped at depth 10; the result is a lower bound, not the full " +
  "blast radius; 16977 of 54269 unresolved attribution site(s) have no indexed target after " +
  "excluding 37292 site(s) classified as builtin, runtime-global, external-import, no-namesake, " +
  "or module-path; classification does not prove complete source coverage";

function radius(over: Partial<ComposedBlastRadius> = {}): ComposedBlastRadius {
  return {
    available: true,
    reason: null,
    seeds: ["src/a.ts"],
    unmatched_targets: [],
    total_impacted: 4740,
    overlap_possible: false,
    layers: [
      {
        depth: 1,
        nodes: [
          "Sources/ExpanderEngine/AI/SelectionReader.swift::SelectionReader.readSelectionImpl",
        ],
        node_count: 4,
        nodes_omitted: 17,
        lowest_confidence: 1,
      },
    ],
    layers_truncated: false,
    walk_incomplete: null,
    unavailable_seeds: [],
    cancelled_seeds: 0,
    ...over,
  };
}

const html = (over: Partial<ComposedBlastRadius> = {}, open = false) =>
  render(BlastRadiusPanel, { props: { blast: radius(over), open } }).body;

describe("BlastRadiusPanel is glanceable by default", () => {
  it("collapses to a single sentence and keeps the engine essay off screen", () => {
    const body = html({ walk_incomplete: KERNEL_ESSAY });
    expect(body).toContain("Reaches at least 4,740 symbols within 1 hop.");
    // The specific regression: 60 words of kernel prose above the code.
    expect(body).not.toContain("unresolved attribution site");
    expect(body).not.toContain("classification does not prove");
  });

  it("still states the qualification while collapsed — detail folds, caveats do not", () => {
    const body = html({ walk_incomplete: KERNEL_ESSAY });
    expect(body).toContain("at least");
    expect(body).toMatch(/aria-expanded="false"/);
  });

  it("an unavailable radius says so on the collapsed row, never reading as zero", () => {
    const body = html({ available: false, reason: "no index", total_impacted: 0 });
    expect(body).toContain("not the same as no impact");
  });

  it("an interrupted walk is not dressed as a finished count", () => {
    const body = html({
      cancelled_seeds: 1,
      unavailable_seeds: [{ seed: "src/x.ts", reason: QUERY_CANCELLED_REASON }],
    });
    expect(body).toContain("Interrupted after reaching");
  });

  it("a complete answer carries no hedge and no confidence chip", () => {
    const body = html();
    expect(body).toContain("Reaches 4,740 symbols within 1 hop.");
    expect(body).not.toContain("at least");
    expect(body).not.toContain("APPROXIMATE");
  });
});

describe("BlastRadiusPanel expanded disclosure", () => {
  it("reports node_count and the unsampled remainder, not only the sample", () => {
    const body = html({}, true);
    // node_count, which the sample length alone would understate as 1.
    expect(body).toContain("4");
    expect(body).toContain("17");
    expect(body).toContain("not sampled");
  });

  it("puts the symbol before the file, which is what the right-hand clip ate", () => {
    const body = html({}, true);
    const symbol = body.indexOf("SelectionReader.readSelectionImpl");
    const file = body.indexOf("SelectionReader.swift<");
    expect(symbol).toBeGreaterThanOrEqual(0);
    expect(file).toBeGreaterThan(symbol);
    // The full path stays reachable as a tooltip rather than being discarded.
    expect(body).toContain("Sources/ExpanderEngine/AI/SelectionReader.swift");
  });

  it("discloses the engine's own account once expanded", () => {
    const body = html({ walk_incomplete: KERNEL_ESSAY }, true);
    expect(body).toContain("What the engine reported");
    expect(body).toContain("unresolved attribution site");
  });

  it("names unmatched seeds and both kinds of refusal", () => {
    const body = html(
      {
        unmatched_targets: ["src/ghost.ts"],
        cancelled_seeds: 1,
        unavailable_seeds: [
          { seed: "src/x.ts", reason: QUERY_CANCELLED_REASON },
          { seed: "src/y.ts", reason: "not indexed" },
        ],
      },
      true,
    );
    expect(body).toContain("Not in the index");
    expect(body).toContain("src/ghost.ts");
    expect(body).toContain("partial answer");
    expect(body).toContain("refused layered impact");
  });

  it("shows a loading state rather than an empty frame", () => {
    const body = render(BlastRadiusPanel, { props: { blast: null, loading: true } }).body;
    expect(body).toContain("Measuring what this change reaches");
  });

  it("renders nothing at all when there is no radius and no fetch", () => {
    const body = render(BlastRadiusPanel, { props: { blast: null } }).body;
    expect(body).not.toContain("Reaches");
    expect(body).not.toContain("Measuring");
  });
});

describe("BlastRadiusPanel source contracts", () => {
  it("never hands a raw engine string to a tooltip, and owns no rung control", () => {
    expect(blastSource).toContain("tooltipWalkIncomplete");
    expect(blastSource).toContain("boundedJoin(layer.nodes");
    expect(blastSource).not.toContain("title={blast.walk_incomplete}");
    expect(blastSource).not.toContain("title={layer.nodes.join");
    expect(blastSource).not.toContain("minRung");
    expect(blastSource).not.toContain("RungFilter");
  });

  it("derives its summary rather than re-deciding confidence inline", () => {
    // A second, divergent confidence ladder in the markup is exactly how the
    // headline and the colour come to disagree.
    expect(blastSource).toContain("blastGlance");
    expect(blastSource).toContain("confidenceLabel");
  });
});

describe("RungFilterControl", () => {
  it("hides when layered impact is active", () => {
    expect(rung).toContain("shouldShowRungControl");
    expect(rung).toContain("layeredImpactActive");
    expect(rung).toContain("rungHistogramLine");
  });
});
