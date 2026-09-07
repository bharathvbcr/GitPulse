import { describe, expect, it } from "vitest";
import {
  buildCodeGraphModel,
  normalizeCounts,
  truncationLegend,
} from "./graphPayload";
import type { GraphVizPayload } from "./types";

describe("normalizeCounts", () => {
  it("treats shown < total as truncated even if the flag is false", () => {
    const counts = normalizeCounts({
      nodes_shown: 2,
      nodes_total: 10,
      nodes_truncated: false,
      max_nodes: 2,
    });
    expect(counts?.nodes_truncated).toBe(true);
  });

  it("preserves an honest untruncated sample", () => {
    const counts = normalizeCounts({
      nodes_shown: 5,
      nodes_total: 5,
      nodes_truncated: false,
    });
    expect(counts?.nodes_truncated).toBe(false);
  });
});

describe("truncationLegend", () => {
  it("never labels a truncated sample as just N nodes", () => {
    const legend = truncationLegend({
      nodes_shown: 1500,
      nodes_total: 12000,
      nodes_truncated: true,
      links_shown: 4000,
      links_total: 50000,
      max_nodes: 1500,
    });
    expect(legend.nodesTruncated).toBe(true);
    expect(legend.nodesLabel).toBe("1500 of 12000 nodes");
    expect(legend.nodesLabel).not.toMatch(/^1500 nodes$/);
    expect(legend.honesty).toContain("not the whole graph");
    expect(legend.honesty).toContain("cap 1500");
    expect(legend.linksLabel).toBe("4000 of 50000 links");
  });

  it("omits honesty when the sample is complete", () => {
    const legend = truncationLegend({
      nodes_shown: 12,
      nodes_total: 12,
      nodes_truncated: false,
      links_shown: 20,
      links_total: 20,
    });
    expect(legend.nodesTruncated).toBe(false);
    expect(legend.nodesLabel).toBe("12 nodes");
    expect(legend.honesty).toBeNull();
  });

  it("surfaces map-preview coverage without inventing node truncation", () => {
    const legend = truncationLegend(null, {
      indexed_total: 400,
      in_subsystems: 310,
    });
    expect(legend.coverageLabel).toBe("310 of 400 indexed files in subsystems");
    expect(legend.nodesTruncated).toBe(false);
  });
});

describe("buildCodeGraphModel", () => {
  const truncatedPayload: GraphVizPayload = {
    level: "file",
    generation_id: 9,
    communities: { core: 2, ui: 1 },
    counts: {
      nodes_shown: 2,
      nodes_total: 5,
      nodes_truncated: true,
      links_shown: 1,
      links_total: 4,
      max_nodes: 2,
    },
    nodes: [
      {
        id: "a.ts",
        name: "a.ts",
        kind: "file",
        path: "a.ts",
        community: "core",
        degree: 3,
      },
      {
        id: "b.ts",
        name: "b.ts",
        kind: "file",
        path: "b.ts",
        community: "ui",
        degree: 1,
      },
    ],
    links: [{ source: "a.ts", target: "b.ts", kind: "imports", confidence: 0.9 }],
  };

  it("maps payload nodes/links and keeps truncation counts", () => {
    const model = buildCodeGraphModel(truncatedPayload, 640, 400);
    expect(model.nodes).toHaveLength(2);
    expect(model.links).toHaveLength(1);
    expect(model.links[0].sourceIndex).toBe(0);
    expect(model.links[0].targetIndex).toBe(1);
    expect(model.counts?.nodes_truncated).toBe(true);
    expect(model.counts?.nodes_shown).toBe(2);
    expect(model.counts?.nodes_total).toBe(5);
    expect(model.level).toBe("file");
    expect(model.generationId).toBe("9");
    expect(model.communities.some((c) => c.name === "core")).toBe(true);
    // Laid out with finite coordinates.
    expect(Number.isFinite(model.nodes[0].x)).toBe(true);
    expect(Number.isFinite(model.nodes[0].y)).toBe(true);
  });

  it("honours payload coordinates when provided", () => {
    const model = buildCodeGraphModel(
      {
        ...truncatedPayload,
        nodes: [
          { ...truncatedPayload.nodes[0], x: 10, y: 20 },
          { ...truncatedPayload.nodes[1], x: 30, y: 40 },
        ],
      },
      640,
      400,
    );
    expect(model.nodes[0].x).toBe(10);
    expect(model.nodes[0].y).toBe(20);
    expect(model.nodes[1].x).toBe(30);
    expect(model.nodes[1].y).toBe(40);
  });

  it("drops links whose endpoints were truncated away", () => {
    const model = buildCodeGraphModel({
      ...truncatedPayload,
      links: [
        { source: "a.ts", target: "b.ts", kind: "imports" },
        { source: "a.ts", target: "missing.ts", kind: "imports" },
      ],
    });
    expect(model.links).toHaveLength(1);
    expect(model.links[0].target).toBe("b.ts");
  });
});
