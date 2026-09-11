import { describe, expect, it } from "vitest";
import { composeLayeredImpacts } from "./blastCompose";
import {
  CODEINTEL_FANOUT_CAP,
  FANOUT_OMITTED_REASON,
  capFanout,
  omittedLayeredImpact,
} from "./fanout";

describe("capFanout", () => {
  it("keeps the first cap items and reports the omitted remainder", () => {
    const items = Array.from({ length: CODEINTEL_FANOUT_CAP + 3 }, (_, i) => `f${i}.ts`);
    const result = capFanout(items);
    expect(result.kept).toHaveLength(CODEINTEL_FANOUT_CAP);
    expect(result.omitted).toEqual(["f16.ts", "f17.ts", "f18.ts"]);
    expect(result.truncated).toBe(true);
    expect(result.total).toBe(19);
  });

  it("does not invent truncation under the cap", () => {
    expect(capFanout(["a.ts", "b.ts"])).toEqual({
      kept: ["a.ts", "b.ts"],
      omitted: [],
      truncated: false,
      total: 2,
    });
  });
});

describe("omittedLayeredImpact", () => {
  it("is unavailable, not cancelled, and not unmatched", () => {
    const result = composeLayeredImpacts(
      [
        {
          available: true,
          reason: null,
          edges: {
            available: true,
            items: [],
            total: 0,
            shown: 0,
            truncated: false,
          },
          blast_radius: {
            seeds: ["kept.ts"],
            unmatched_targets: [],
            total_impacted: 2,
            layers: {
              available: true,
              items: [{ depth: 1, nodes: ["Foo"], node_count: 2, nodes_omitted: 0 }],
              total: 1,
              shown: 1,
              truncated: false,
            },
          },
        },
        omittedLayeredImpact("late.ts"),
      ],
      ["kept.ts", "late.ts"],
    );
    expect(result.cancelled_seeds).toBe(0);
    expect(result.unavailable_seeds).toEqual([
      { seed: "late.ts", reason: FANOUT_OMITTED_REASON },
    ]);
    expect(result.unmatched_targets).toEqual([]);
  });
});
