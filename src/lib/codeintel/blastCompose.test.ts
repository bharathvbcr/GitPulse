import { describe, expect, it } from "vitest";
import { composeLayeredImpacts, emptyComposedBlast } from "./blastCompose";
import type { CodeintelLayeredImpact } from "./types";

function layered(opts: {
  seed: string;
  node_count: number;
  nodes_omitted: number;
  unmatched?: string[];
  walk_incomplete?: string;
  available?: boolean;
  reason?: string | null;
}): CodeintelLayeredImpact {
  const available = opts.available ?? true;
  return {
    available,
    reason: opts.reason ?? null,
    edges: {
      available,
      reason: opts.reason ?? null,
      items: [],
      total: 0,
      shown: 0,
      truncated: false,
    },
    blast_radius: {
      seeds: [opts.seed],
      unmatched_targets: opts.unmatched ?? [],
      total_impacted: available ? opts.node_count : 0,
      layers: {
        available,
        reason: opts.reason ?? null,
        items: available
          ? [
              {
                depth: 1,
                nodes: ["Foo", "Bar"],
                node_count: opts.node_count,
                nodes_omitted: opts.nodes_omitted,
                lowest_confidence: 0.9,
              },
            ]
          : [],
        total: available ? 1 : 0,
        shown: available ? 1 : 0,
        truncated: false,
        ...(opts.walk_incomplete
          ? { walk_incomplete: opts.walk_incomplete }
          : {}),
      },
    },
  };
}

describe("composeLayeredImpacts", () => {
  it("sums node_count and nodes_omitted per hop and keeps unmatched + walk honesty", () => {
    const result = composeLayeredImpacts(
      [
        layered({
          seed: "a.ts",
          node_count: 12,
          nodes_omitted: 4,
          unmatched: ["ghost.ts"],
          walk_incomplete: "unattributed calls",
        }),
        layered({
          seed: "b.ts",
          node_count: 5,
          nodes_omitted: 2,
        }),
      ],
      ["a.ts", "b.ts"],
    );

    expect(result.available).toBe(true);
    expect(result.unmatched_targets).toEqual(["ghost.ts"]);
    expect(result.walk_incomplete).toContain("unattributed calls");
    expect(result.layers[0].node_count).toBe(17);
    expect(result.layers[0].nodes_omitted).toBe(6);
    expect(result.total_impacted).toBe(17);
    expect(result.overlap_possible).toBe(true);
  });

  it("does not present an empty unavailable result as zero impact", () => {
    const result = composeLayeredImpacts([
      layered({
        seed: "x.ts",
        node_count: 0,
        nodes_omitted: 0,
        unmatched: ["x.ts"],
        available: false,
        reason: "map missing",
      }),
    ]);
    expect(result.available).toBe(false);
    expect(result.reason).toMatch(/map missing/);
    expect(emptyComposedBlast().available).toBe(false);
  });
});
