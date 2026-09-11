import { describe, expect, it } from "vitest";
import { composeLayeredImpacts, emptyComposedBlast } from "./blastCompose";
import type { CodeintelLayeredImpact } from "./types";

/** Kernel-shaped coverage essay attached to every walk on a holey corpus. */
const COVERAGE =
  "28637 of 69195 unresolved attribution site(s) have no indexed target after excluding 40548 known builtin, runtime-global, and external-import site(s); these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies";

function seedWalk(unrecorded: number): string {
  return `the walk did not complete: stopped at depth 10, ${unrecorded} traversed edges unrecorded; the result is a lower bound, not the full blast radius; ${COVERAGE}`;
}

function layered(opts: {
  seed: string;
  node_count: number;
  nodes_omitted: number;
  unmatched?: string[];
  walk_incomplete?: string;
  edges_walk_incomplete?: string;
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
      ...(opts.edges_walk_incomplete
        ? { walk_incomplete: opts.edges_walk_incomplete }
        : opts.walk_incomplete
          ? { walk_incomplete: opts.walk_incomplete }
          : {}),
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

  it("does not concatenate per-seed walk essays that differ only by unrecorded-edge counts", () => {
    const available = Array.from({ length: 25 }, (_, i) =>
      layered({
        seed: `src/f${i}.go`,
        node_count: 10,
        nodes_omitted: 2,
        walk_incomplete: seedWalk(700 + i * 500),
      }),
    );
    const unmatched = Array.from({ length: 179 }, (_, i) =>
      layered({
        seed: i === 0 ? "README.md" : `doc/${i}.md`,
        node_count: 0,
        nodes_omitted: 0,
        unmatched: [i === 0 ? "README.md" : `doc/${i}.md`],
        available: false,
        reason: "has no indexed traversal start",
        walk_incomplete: COVERAGE,
      }),
    );

    const result = composeLayeredImpacts([...available, ...unmatched]);
    const text = result.walk_incomplete ?? "";

    expect(result.available).toBe(true);
    expect(result.unmatched_targets).toHaveLength(179);
    expect(text.length).toBeLessThan(1200);
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text.split("the walk did not complete").length - 1).toBe(1);
    expect(text).toMatch(/700\u2013/);
    expect(text).toContain("25 seeds");
    expect(text).toContain("unrecorded");
  });

  it("folds per-seed unavailable reasons instead of keeping only the first", () => {
    const unmatched = Array.from({ length: 25 }, (_, i) =>
      layered({
        seed: `doc/${i}.md`,
        node_count: 0,
        nodes_omitted: 0,
        unmatched: [`doc/${i}.md`],
        available: false,
        reason: seedWalk(100 + i),
      }),
    );
    const result = composeLayeredImpacts(unmatched);
    const text = result.reason ?? "";
    expect(result.available).toBe(false);
    expect(text.length).toBeLessThan(1200);
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text).toContain("25 seeds");
  });

  it("folds an already-joined walk_incomplete blob from a previous compose", () => {
    const blob = Array.from({ length: 12 }, (_, i) => seedWalk(100 + i)).join(" · ");
    const result = composeLayeredImpacts([
      layered({
        seed: "a.go",
        node_count: 4,
        nodes_omitted: 0,
        walk_incomplete: blob,
      }),
    ]);
    const text = result.walk_incomplete ?? "";
    expect(text.length).toBeLessThan(1200);
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text).toContain("12 seeds");
  });

  it("does not turn non-finite hop totals into NaN impacted", () => {
    const result = composeLayeredImpacts([
      layered({ seed: "a.ts", node_count: Number.NaN, nodes_omitted: 1 }),
      layered({ seed: "b.ts", node_count: Number.POSITIVE_INFINITY, nodes_omitted: 2 }),
      layered({ seed: "c.ts", node_count: 4, nodes_omitted: 0 }),
    ]);
    expect(Number.isFinite(result.total_impacted)).toBe(true);
    expect(result.total_impacted).toBe(4);
    expect(Number.isFinite(result.layers[0]?.node_count)).toBe(true);
    expect(result.layers[0]?.node_count).toBe(4);
  });

  it("names cancelled seeds instead of treating them as an unindexed map", () => {
    const result = composeLayeredImpacts([
      layered({
        seed: "a.ts",
        node_count: 3,
        nodes_omitted: 0,
      }),
      ...Array.from({ length: 12 }, (_, i) =>
        layered({
          seed: `late/${i}.ts`,
          node_count: 0,
          nodes_omitted: 0,
          unmatched: [`late/${i}.ts`],
          available: false,
          reason: "query cancelled",
        }),
      ),
    ]);
    expect(result.available).toBe(true);
    expect(result.cancelled_seeds).toBe(12);
    expect(result.unavailable_seeds).toHaveLength(12);
    expect(result.unmatched_targets).toEqual([]);
  });

  it("keeps unmatched from unindexed seeds while excluding cancelled ones", () => {
    const result = composeLayeredImpacts([
      layered({ seed: "ok.ts", node_count: 2, nodes_omitted: 0 }),
      layered({
        seed: "README.md",
        node_count: 0,
        nodes_omitted: 0,
        unmatched: ["README.md"],
        available: false,
        reason: "impact unavailable",
      }),
      layered({
        seed: "late.ts",
        node_count: 0,
        nodes_omitted: 0,
        unmatched: ["late.ts"],
        available: false,
        reason: "query cancelled",
      }),
    ]);
    expect(result.unmatched_targets).toEqual(["README.md"]);
    expect(result.cancelled_seeds).toBe(1);
    expect(result.unavailable_seeds).toHaveLength(2);
  });

  it("folds per-seed unavailable reasons rather than dumping the coverage essay", () => {
    const result = composeLayeredImpacts([
      layered({
        seed: "README.md",
        node_count: 0,
        nodes_omitted: 0,
        unmatched: ["README.md"],
        available: false,
        reason: seedWalk(1),
      }),
    ]);
    const reason = result.unavailable_seeds[0]?.reason ?? "";
    expect(reason.length).toBeLessThan(1200);
    expect(reason.split("repository-wide counts").length - 1).toBe(1);
  });
});
